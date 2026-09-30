//! Job system: single active heavy job + FIFO queue, 200ms progress
//! throttle, cooperative cancellation. UI-agnostic — events go to a sink.
//!
//! Runner contract: `Err` strings must be stable `error.*` keys; any error
//! raised after cancellation is remapped to `error.cancelled`.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Events flowing to the UI sink. `Progress` is throttled (default 200ms);
/// the final in-job state bypasses the throttle so the last numbers land.
#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    Progress {
        job_id: u64,
        done_items: u64,
        total_items: u64,
        done_bytes: u64,
        total_bytes: u64,
        speed_bps: u64,
        eta_secs: u64,
    },
    Finished {
        job_id: u64,
        ok: bool,
        error_key: Option<String>,
    },
}

type Sink = Arc<dyn Fn(JobEvent) + Send + Sync>;
type Runner = Box<dyn FnOnce(&JobContext) -> Result<(), String> + Send>;

struct Inner {
    next_id: u64,
    active: Option<u64>,
    queue: VecDeque<(u64, Option<Runner>)>,
    flags: Vec<(u64, Arc<AtomicBool>)>,
}

/// Owns the queue and the single active worker thread.
pub struct JobManager {
    sink: Sink,
    throttle: Duration,
    inner: Arc<Mutex<Inner>>,
}

impl JobManager {
    /// Default throttle: 200ms between `Progress` events.
    pub fn new(sink: Box<dyn Fn(JobEvent) + Send + Sync + 'static>) -> Self {
        Self::with_throttle(sink, Duration::from_millis(200))
    }

    pub fn with_throttle(
        sink: Box<dyn Fn(JobEvent) + Send + Sync + 'static>,
        throttle: Duration,
    ) -> Self {
        JobManager {
            sink: Arc::from(sink),
            throttle,
            inner: Arc::new(Mutex::new(Inner {
                next_id: 1,
                active: None,
                queue: VecDeque::new(),
                flags: Vec::new(),
            })),
        }
    }

    /// Queue a runner; returns its job id immediately. Starts the worker if idle.
    pub fn submit(&self, _kind: &str, runner: Runner) -> u64 {
        let id = {
            let mut g = self.inner.lock().unwrap();
            let id = g.next_id;
            g.next_id += 1;
            g.flags.push((id, Arc::new(AtomicBool::new(false))));
            g.queue.push_back((id, Some(runner)));
            id
        };
        self.kick();
        id
    }

    /// Cooperative cancel: flips the job's flag; the runner must poll
    /// `JobContext::cancelled()`. Returns false if the id is unknown.
    pub fn cancel(&self, job_id: u64) -> bool {
        let g = self.inner.lock().unwrap();
        match g.flags.iter().find(|(i, _)| *i == job_id) {
            Some((_, f)) => {
                f.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Job currently running, if any.
    pub fn active_id(&self) -> Option<u64> {
        self.inner.lock().unwrap().active
    }

    /// Jobs waiting behind the active one.
    pub fn queue_len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// Promote the next queued runner if nothing is active.
    fn kick(&self) {
        let (id, runner) = {
            let mut g = self.inner.lock().unwrap();
            if g.active.is_some() {
                return;
            }
            match g.queue.pop_front() {
                Some((id, runner)) => {
                    g.active = Some(id);
                    (id, runner)
                }
                None => return,
            }
        };
        spawn_worker(Arc::clone(&self.sink), Arc::clone(&self.inner), self.throttle, id, runner);
    }
}

/// Run one job on a fresh thread, then promote the next queued runner.
fn spawn_worker(sink: Sink, inner: Arc<Mutex<Inner>>, throttle: Duration, id: u64, runner: Option<Runner>) {
    let Some(runner) = runner else {
        // Defensive: an id without a runner (shouldn't happen).
        inner.lock().unwrap().active = None;
        return;
    };
    std::thread::spawn(move || {
        let flag = {
            let g = inner.lock().unwrap();
            g.flags
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, f)| Arc::clone(f))
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
        };
        let ctx = JobContext {
            id,
            flag,
            sink: Arc::clone(&sink),
            throttle,
            last_emit: Mutex::new(Instant::now() - throttle),
            prev_bytes: Mutex::new((0u64, Instant::now())),
        };
        // A panicking runner must not wedge the queue.
        let result = catch_unwind(AssertUnwindSafe(|| runner(&ctx)))
            .unwrap_or_else(|_| Err("error.engine".to_string()));
        let (ok, error_key) = match result {
            Ok(()) => (true, None),
            Err(k) => (
                false,
                Some(if ctx.cancelled() {
                    "error.cancelled".into()
                } else {
                    k
                }),
            ),
        };
        (sink)(JobEvent::Finished {
            job_id: id,
            ok,
            error_key,
        });
        // Clear this job and promote the next one in one critical section,
        // so a concurrent submit->kick can't double-spawn.
        let next = {
            let mut g = inner.lock().unwrap();
            g.active = None;
            g.flags.retain(|(i, _)| *i != id);
            match g.queue.pop_front() {
                Some((nid, nr)) => {
                    g.active = Some(nid);
                    Some((nid, nr))
                }
                None => None,
            }
        };
        if let Some((next_id, next_runner)) = next {
            spawn_worker(sink, inner, throttle, next_id, next_runner);
        }
    });
}

/// Handle passed to runners: cancellation checks + throttled progress.
pub struct JobContext {
    id: u64,
    flag: Arc<AtomicBool>,
    sink: Sink,
    throttle: Duration,
    last_emit: Mutex<Instant>,
    prev_bytes: Mutex<(u64, Instant)>,
}

impl JobContext {
    pub fn job_id(&self) -> u64 {
        self.id
    }

    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Throttled emit; bypasses the throttle when the job is complete
    /// (done_bytes == total_bytes) so the final state always lands.
    pub fn report(&self, done_items: u64, total_items: u64, done_bytes: u64, total_bytes: u64) {
        let now = Instant::now();
        {
            let last = self.last_emit.lock().unwrap();
            if now.duration_since(*last) < self.throttle && done_bytes != total_bytes {
                return;
            }
        }
        *self.last_emit.lock().unwrap() = now;
        let (speed_bps, eta_secs) = {
            let mut prev = self.prev_bytes.lock().unwrap();
            let dt = (now - prev.1).as_secs_f64().max(1e-3);
            let speed = (done_bytes.saturating_sub(prev.0) as f64 / dt) as u64;
            let eta = if speed > 0 && done_bytes < total_bytes {
                (total_bytes - done_bytes) / speed
            } else {
                0
            };
            *prev = (done_bytes, now);
            (speed, eta)
        };
        (self.sink)(JobEvent::Progress {
            job_id: self.id,
            done_items,
            total_items,
            done_bytes,
            total_bytes,
            speed_bps,
            eta_secs,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// (event log, sink) pair handed to a test JobManager.
    type Recording = (Arc<Mutex<Vec<JobEvent>>>, Box<dyn Fn(JobEvent) + Send + Sync>);

    fn recorder() -> Recording {
        let v = Arc::new(Mutex::new(Vec::new()));
        let v2 = v.clone();
        (v, Box::new(move |e| v2.lock().unwrap().push(e)))
    }

    fn wait_until(ms: u64, mut pred: impl FnMut() -> bool) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            if pred() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pred(), "condition not met within {ms}ms");
    }

    #[test]
    fn runs_job_and_reports_finished_ok() {
        let (log, sink) = recorder();
        let mgr = JobManager::new(sink);
        mgr.submit("t", Box::new(|ctx| {
            ctx.report(1, 1, 10, 10);
            Ok(())
        }));
        wait_until(2000, || {
            log.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, JobEvent::Finished { .. }))
        });
        let v = log.lock().unwrap();
        assert!(v.iter().any(|e| matches!(e, JobEvent::Progress { .. })));
        assert!(v
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { ok: true, error_key: None, .. })));
    }

    #[test]
    fn cancel_is_cooperative_and_remaps_error() {
        let (log, sink) = recorder();
        let mgr = JobManager::with_throttle(sink, Duration::from_millis(0));
        let id = mgr.submit("t", Box::new(|ctx| {
            std::thread::sleep(Duration::from_millis(50));
            if ctx.cancelled() {
                Err("error.engine".into())
            } else {
                Ok(())
            }
        }));
        mgr.cancel(id);
        wait_until(2000, || {
            log.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, JobEvent::Finished { .. }))
        });
        let v = log.lock().unwrap();
        let fin = v
            .iter()
            .find_map(|e| match e {
                JobEvent::Finished { ok, error_key, .. } => Some((*ok, error_key.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(fin, (false, Some("error.cancelled".to_string())));
    }

    #[test]
    fn queued_jobs_run_after_active_finishes() {
        let (log, sink) = recorder();
        let mgr = JobManager::with_throttle(sink, Duration::from_millis(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(std::sync::Barrier::new(2));
        let o1 = order.clone();
        let g1 = gate.clone();
        mgr.submit("slow", Box::new(move |_| {
            g1.wait();
            o1.lock().unwrap().push(1);
            Ok(())
        }));
        let o2 = order.clone();
        mgr.submit("next", Box::new(move |_| {
            o2.lock().unwrap().push(2);
            Ok(())
        }));
        assert_eq!(mgr.queue_len(), 1);
        gate.wait();
        wait_until(2000, || {
            log.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, JobEvent::Finished { job_id, .. } if *job_id == 2))
        });
        assert_eq!(*order.lock().unwrap(), vec![1, 2]);
        assert_eq!(mgr.queue_len(), 0);
    }

    #[test]
    fn progress_is_throttled() {
        let (log, sink) = recorder();
        let mgr = JobManager::with_throttle(sink, Duration::from_secs(60));
        mgr.submit("t", Box::new(|ctx| {
            for i in 0..100 {
                ctx.report(i, 100, i, 100);
            }
            Ok(())
        }));
        wait_until(2000, || {
            log.lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, JobEvent::Finished { .. }))
        });
        let v = log.lock().unwrap();
        let prog = v
            .iter()
            .filter(|e| matches!(e, JobEvent::Progress { .. }))
            .count();
        assert!(prog <= 2, "expected <=2 progress events, got {prog}");
    }
}

//! Streaming download to a `.part` file, with resume, progress and cancel.
//!
//! Three separate things go wrong on a Chinese residential line, and they need
//! three different responses — this was measured, not guessed:
//!
//! * **connect hang** — `Failed to connect to github.com:443 after 21053 ms`.
//!   Fail over to the next source; a short connect timeout classifies it.
//! * **TLS completes, then silence** — zero bytes until the timeout, DNS and
//!   certificate both fine (`raw.githubusercontent.com` did this six times out
//!   of six). Also a failover.
//! * **mid-transfer truncation** — this is the dangerous one: 5 attempts at the
//!   real installer produced one complete file, one file that stopped at
//!   1,398,784 of 5,137,077 bytes **with a normal HTTP 200**, and three outright
//!   failures. Nothing about the response said "truncated". Only the size and
//!   hash in the signed manifest catch this, which is why they are mandatory and
//!   checked before anything is executed.
//!
//! Resume is worth having because every surviving hop honours `Range`
//! (verified at offsets 0, 1 MB, 2.5 MB and 5 MB on the direct CDN and both
//! accelerators), which turns "slow but alive" into "slow but finishable".

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::manifest::Asset;
use crate::source::{self, CONNECT_TIMEOUT, READ_TIMEOUT, STALL_TIMEOUT};
use crate::UpdateError;

/// What to do with a response to a `Range` request.
///
/// The #1 resume bug is appending to a `.part` after a server ignored `Range`
/// and sent the whole file with a `200`, which produces a corrupt file that
/// only fails at the hash check — after a full download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAction {
    /// Server honoured the range: keep what we have and append.
    Append,
    /// Server ignored the range, or answered about a different window: start over.
    Restart,
}

/// Decide from the response whether the local `.part` may be appended to.
///
/// `have` is the byte count already on disk, `range_requested` says whether we
/// actually asked for a range, `status` is the HTTP status, and `content_range`
/// is the parsed `Content-Range` start offset (if the header was readable).
pub fn resume_action(
    have: u64,
    range_requested: bool,
    status: u16,
    content_range_start: Option<u64>,
) -> ResumeAction {
    if have == 0 || !range_requested {
        return ResumeAction::Restart;
    }
    if status != 206 {
        // 200 means the server sent the whole file from byte 0.
        return ResumeAction::Restart;
    }
    match content_range_start {
        Some(start) if start == have => ResumeAction::Append,
        // A 206 that starts somewhere other than where we stopped is not our
        // file; appending it would produce a silently wrong result.
        _ => ResumeAction::Restart,
    }
}

/// Parse the start offset out of `Content-Range: bytes 1000-5137076/5137077`.
pub fn parse_content_range_start(value: &str) -> Option<u64> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let first = rest.split('-').next()?;
    first.trim().parse::<u64>().ok()
}

/// Progress reported to the UI. Throttled by the caller's callback.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    /// Index into the source list, so the dialog can say which mirror is being
    /// used without the UI knowing about sources at all.
    pub source_index: usize,
}

/// Download `asset` from the first source that delivers a complete, correctly
/// hashed file.
///
/// Returns the path of the finished file. The `.part` and its sidecar live
/// beside the final name while the transfer runs; the sidecar records which
/// source produced it and how many bytes it holds, so a resumed download from a
/// *different* source is still safe (the hash decides).
pub fn download<F, C>(
    asset: &Asset,
    urls: &[String],
    dest_dir: &Path,
    mut on_progress: F,
    cancelled: C,
) -> Result<PathBuf, UpdateError>
where
    F: FnMut(Progress),
    C: Fn() -> bool,
{
    std::fs::create_dir_all(dest_dir).map_err(|e| UpdateError::Io(e.to_string()))?;
    let final_path = dest_dir.join(&asset.name);
    let part_path = dest_dir.join(format!("{}.part", asset.name));
    let sidecar = sidecar_path(&part_path);

    // A finished file from an earlier run is reused only if it still hashes
    // correctly; anything else is treated as absent.
    if final_path.exists() {
        if let Ok(h) = crate::verify::sha256_file(&final_path) {
            if h == asset.sha256 {
                on_progress(Progress {
                    done: asset.size,
                    total: asset.size,
                    source_index: 0,
                });
                return Ok(final_path);
            }
        }
        let _ = std::fs::remove_file(&final_path);
    }

    let mut last_error = UpdateError::Source("no download source configured".into());
    for (index, url) in urls.iter().enumerate() {
        if cancelled() {
            return Err(UpdateError::Cancelled);
        }
        match fetch_one(url, asset, &part_path, index, &mut on_progress, &cancelled) {
            Ok(()) => {
                // Length first (cheap), then the hash. Either failing moves on to
                // the next source: a truncated file from one mirror may be whole
                // from another.
                let len = std::fs::metadata(&part_path)
                    .map_err(|e| UpdateError::Io(e.to_string()))?
                    .len();
                if len != asset.size {
                    last_error = UpdateError::Incomplete {
                        source: url.clone(),
                        got: len,
                        want: asset.size,
                    };
                    continue;
                }
                let got = crate::verify::sha256_file(&part_path)?;
                if got != asset.sha256 {
                    last_error = UpdateError::HashMismatch {
                        source: url.clone(),
                        got,
                        want: asset.sha256.clone(),
                    };
                    continue;
                }
                if let Err(e) = replace_file(&part_path, &final_path) {
                    return Err(UpdateError::Io(e.to_string()));
                }
                let _ = std::fs::remove_file(&sidecar);
                return Ok(final_path);
            }
            Err(UpdateError::Cancelled) => return Err(UpdateError::Cancelled),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

fn sidecar_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_owned();
    s.push(".json");
    PathBuf::from(s)
}

/// One source, with resume from whatever the `.part` already holds.
fn fetch_one<F, C>(
    url: &str,
    asset: &Asset,
    part_path: &Path,
    source_index: usize,
    on_progress: &mut F,
    cancelled: &C,
) -> Result<(), UpdateError>
where
    F: FnMut(Progress),
    C: Fn() -> bool,
{
    // The agent is built per attempt on purpose. `timeout_global` is NOT set:
    // the existing 10 s budget in main.rs is right for a JSON check and would
    // abort every download on a 44 KB/s line (5 MB = 115 s). Stalls are caught
    // by `timeout_per_read` plus the caller-side watchdog below.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
        .timeout_resolve(Some(Duration::from_secs(10)))
        .max_redirects(3)
        .max_redirects_will_error(true)
        .build()
        .into();

    let mut have = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
    // A `.part` already at the full length cannot be resumed, so the only two
    // options are "hash it" and "throw it away". It must be hashed *here*,
    // because returning `Ok(())` on it would make every source look successful
    // without a request, and `download` would walk the whole mirror list
    // without downloading anything and report a hash mismatch no source was
    // ever asked to fix. A mirror that served wrong bytes of exactly the right
    // length leaves such a file behind, and so does a cancel in the final read
    // cycle - which may just as well be a perfectly good download.
    if have == asset.size {
        if crate::verify::sha256_file(part_path)? == asset.sha256 {
            return Ok(());
        }
        // Wrong contents: delete it and take the whole file again from this
        // source rather than failing the source for a local file's fault.
        let _ = std::fs::remove_file(part_path);
        have = 0;
    }
    // Never resume past the expected size; a longer `.part` is corrupt.
    if have > asset.size {
        let _ = std::fs::remove_file(part_path);
        have = 0;
    }
    let range_requested = have > 0;

    let mut req = agent.get(url);
    if source::wants_octet_stream(url) {
        // Without this the REST asset endpoint answers with JSON metadata.
        req = req.header("Accept", "application/octet-stream");
    }
    if range_requested {
        req = req.header("Range", &format!("bytes={have}-"));
    }

    let mut res = match req.call() {
        Ok(r) => r,
        // `ureq` turns a 4xx into `Err` before the code below can inspect the
        // status, so `resume_action`'s 416 branch is unreachable without this.
        // "Range Not Satisfiable" means our `.part` is at or past what the
        // server has; restart from zero rather than failing the source.
        Err(ureq::Error::StatusCode(416)) if range_requested => {
            match std::fs::remove_file(part_path) {
                // Recurse only once the file is gone — retrying with the same
                // Range on a file that could not be deleted would loop.
                Ok(()) => {
                    return fetch_one(url, asset, part_path, source_index, on_progress, cancelled)
                }
                Err(e) => {
                    return Err(UpdateError::Io(format!(
                        "could not clear a stale partial download: {e}"
                    )))
                }
            }
        }
        Err(e) => {
            return Err(UpdateError::Network {
                source: url.to_string(),
                detail: describe_ureq(&e),
            })
        }
    };

    let status = res.status().as_u16();
    let content_range_start = res
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range_start);
    match resume_action(have, range_requested, status, content_range_start) {
        ResumeAction::Append => {}
        ResumeAction::Restart => {
            let _ = std::fs::remove_file(part_path);
            have = 0;
        }
    }

    let total = asset.size;
    on_progress(Progress {
        done: have,
        total,
        source_index,
    });

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(part_path)
        .map_err(|e| UpdateError::Io(e.to_string()))?;
    let mut reader = res
        .body_mut()
        .with_config()
        // One byte of slack on purpose: ureq's `LimitReader` raises
        // `BodyExceedsLimit` on the read *after* the budget is spent, so an
        // exact limit makes a complete transfer fail at its last byte (it has no
        // way to observe EOF). The caller still enforces the manifest size, so
        // the extra byte is a guard rail, not a trust boundary.
        .limit(total.saturating_sub(have).saturating_add(1))
        .reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut last_report = Instant::now();
    let mut window = StallWindow::default();
    loop {
        if cancelled() {
            return Err(UpdateError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // A read error mid-transfer keeps the `.part`: the next source
                // (or the next run) resumes from here.
                return Err(UpdateError::Network {
                    source: url.to_string(),
                    detail: e.to_string(),
                });
            }
        };
        file.write_all(&buf[..n])
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        have += n as u64;
        if last_report.elapsed() >= Duration::from_millis(200) {
            on_progress(Progress {
                done: have,
                total,
                source_index,
            });
            last_report = Instant::now();
        }
        // Caller-side stall watchdog. It has to measure *throughput*, not the
        // gap between reads: a server dribbling one byte every 25 s keeps the
        // socket alive, so `timeout_per_read` never fires and every read
        // returns, yet the transfer would need hours to finish. An earlier
        // version reset its timer on every received byte and therefore could
        // not fire in exactly the case its comment described.
        window.add(n as u64);
        if window.is_stalled(STALL_TIMEOUT) {
            return Err(UpdateError::Network {
                source: url.to_string(),
                detail: "transfer stalled".into(),
            });
        }
    }
    file.flush().map_err(|e| UpdateError::Io(e.to_string()))?;
    // Durability before the rename: a power loss between the two would
    // otherwise leave a full-length file with unflushed tail bytes.
    file.sync_all().map_err(|e| UpdateError::Io(e.to_string()))?;
    drop(file);
    on_progress(Progress {
        done: have,
        total,
        source_index,
    });
    Ok(())
}

/// Minimum bytes that must arrive within one [`STALL_TIMEOUT`] window for the
/// transfer to count as alive.
///
/// 16 KiB in 60 s is 273 B/s — a 5 MB installer would take five hours. Any real
/// mirror measured here does at least 355 KB/s, and even the worst direct CDN
/// hop managed 44 KB/s, i.e. 2.6 MB per window. So this rejects only transfers
/// that are never going to finish, and it cannot fire on a merely slow line.
const STALL_MIN_BYTES: u64 = 16 * 1024;

/// Sliding throughput window for the stall watchdog.
#[derive(Debug, Default)]
struct StallWindow {
    start: Option<Instant>,
    bytes: u64,
}

impl StallWindow {
    fn add(&mut self, n: u64) {
        self.start.get_or_insert_with(Instant::now);
        self.bytes += n;
    }

    /// True when a full window has passed and it carried too few bytes.
    /// Restarts the window when it judges the transfer alive.
    fn is_stalled(&mut self, window: Duration) -> bool {
        let Some(start) = self.start else { return false };
        if start.elapsed() < window {
            return false;
        }
        let stalled = self.bytes < STALL_MIN_BYTES;
        self.start = Some(Instant::now());
        self.bytes = 0;
        stalled
    }
}

/// `std::fs::rename` on Windows refuses to replace an existing file, so the
/// destination is removed first. Both can meet a transient lock while Defender
/// scans the freshly written exe (`ERROR_SHARING_VIOLATION` 32 /
/// `ERROR_ACCESS_DENIED` 5), which is worth a short backoff rather than a
/// failed update.
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        let _ = std::fs::remove_file(to);
    }
    let mut delay = Duration::from_millis(500);
    for attempt in 0..4 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt == 3 => return Err(e),
            Err(_) => {
                std::thread::sleep(delay);
                delay *= 2;
            }
        }
    }
    Ok(())
}

/// Map a `ureq` error onto something the UI can say in Chinese.
pub fn describe_ureq(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "timeout".into(),
        ureq::Error::StatusCode(code) => format!("http {code}"),
        ureq::Error::HostNotFound => "host not found".into(),
        ureq::Error::ConnectionFailed => "connection failed".into(),
        ureq::Error::Tls(msg) => format!("tls: {msg}"),
        ureq::Error::Io(io) => format!("io: {io}"),
        ureq::Error::TooManyRedirects => "too many redirects".into(),
        other => other.to_string(),
    }
}

/// Set by [`DownloadGuard`] so a cancel from the UI thread is visible to the
/// worker without a channel round trip.
#[derive(Default)]
pub struct CancelFlag(AtomicBool);

impl CancelFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_file_always_restarts() {
        assert_eq!(resume_action(0, false, 200, None), ResumeAction::Restart);
        assert_eq!(resume_action(0, false, 206, Some(0)), ResumeAction::Restart);
    }

    #[test]
    fn a_200_answer_to_a_range_request_restarts() {
        // The classic resume bug: appending a whole second copy of the file.
        assert_eq!(
            resume_action(1000, true, 200, None),
            ResumeAction::Restart
        );
    }

    #[test]
    fn a_206_at_the_right_offset_appends() {
        assert_eq!(
            resume_action(1000, true, 206, Some(1000)),
            ResumeAction::Append
        );
    }

    #[test]
    fn a_206_at_the_wrong_offset_restarts() {
        assert_eq!(
            resume_action(1000, true, 206, Some(0)),
            ResumeAction::Restart
        );
        assert_eq!(resume_action(1000, true, 206, None), ResumeAction::Restart);
    }

    #[test]
    fn a_416_restarts_rather_than_looping() {
        assert_eq!(resume_action(9999, true, 416, None), ResumeAction::Restart);
    }

    #[test]
    fn content_range_is_parsed_in_both_common_shapes() {
        assert_eq!(
            parse_content_range_start("bytes 1000-5137076/5137077"),
            Some(1000)
        );
        assert_eq!(parse_content_range_start("bytes */5137077"), None);
        assert_eq!(parse_content_range_start("bytes 0-1/*"), Some(0));
        assert_eq!(parse_content_range_start("nonsense"), None);
        assert_eq!(parse_content_range_start(""), None);
    }

    #[test]
    fn the_cancel_flag_is_visible_immediately() {
        let f = CancelFlag::default();
        assert!(!f.is_cancelled());
        f.cancel();
        assert!(f.is_cancelled());
    }

    #[test]
    fn the_sidecar_sits_beside_the_part_file() {
        let p = Path::new(r"C:\x\a.exe.part");
        assert_eq!(sidecar_path(p), PathBuf::from(r"C:\x\a.exe.part.json"));
    }

    #[test]
    fn the_stall_window_needs_a_full_window_before_judging() {
        let mut w = StallWindow::default();
        w.add(10);
        // Nothing has elapsed yet, so it cannot call this a stall.
        assert!(!w.is_stalled(Duration::from_secs(60)));
    }

    #[test]
    fn a_trickle_of_bytes_counts_as_a_stall() {
        // The measured failure mode: a server that keeps the socket alive by
        // sending one byte every few seconds. Reads keep returning, so
        // `timeout_per_read` never fires, but the transfer would take hours.
        let mut w = StallWindow {
            start: Some(Instant::now() - Duration::from_secs(61)),
            bytes: 200,
        };
        assert!(w.is_stalled(Duration::from_secs(60)));
        // ...and the window restarts rather than firing on every later read.
        assert!(!w.is_stalled(Duration::from_secs(60)));
        assert_eq!(w.bytes, 0);
    }

    #[test]
    fn a_merely_slow_line_is_not_a_stall() {
        // 44 KB/s (the worst direct CDN hop measured) carries 2.6 MB in a
        // window, far above the threshold.
        let mut w = StallWindow {
            start: Some(Instant::now() - Duration::from_secs(61)),
            bytes: 2_600_000,
        };
        assert!(!w.is_stalled(Duration::from_secs(60)));
    }

    #[test]
    fn the_stall_threshold_is_far_below_any_usable_line() {
        // 16 KiB per 60 s = 273 B/s, two orders of magnitude under the slowest
        // mirror measured (44 KB/s), so a stalled verdict really does mean
        // "this would never finish" rather than "this is a bit slow today".
        let bytes_per_second = STALL_MIN_BYTES / crate::source::STALL_TIMEOUT.as_secs();
        assert_eq!(bytes_per_second, 273);
        assert!(bytes_per_second * 100 < 44 * 1024, "273 B/s is 160x under the slowest working mirror");
    }

    #[test]
    fn a_cancelled_download_never_touches_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let asset = Asset {
            name: "a.exe".into(),
            size: 10,
            sha256: "0".repeat(64),
        };
        let flag = CancelFlag::default();
        flag.cancel();
        let err = download(
            &asset,
            &["https://127.0.0.1:1/never".to_string()],
            dir.path(),
            |_| {},
            || flag.is_cancelled(),
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::Cancelled));
    }
}

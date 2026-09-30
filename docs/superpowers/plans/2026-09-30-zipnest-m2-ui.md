# ZipNest M2 主界面 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tauri 2 壳 + 归档浏览器 + 解压 + 进度/密码/错误闭环，形成可用原型。

**Architecture:** 三层——`archive-jobs`（通用任务管理：单活跃+排队+200ms 节流+协作取消）、`zipnest-ipc`（纯逻辑服务层：归档注册表 + open/list/read/extract 编排，事件经 emit 回调）、`apps/zipnest`（Tauri glue + React/Fluent 2 前端）。Rust 层零 UI 依赖可独立测试；前端一切经 IPC。

**Tech Stack:** Tauri 2（npm 侧 `@tauri-apps/cli`，不装 cargo tauri-cli）、React 18 + TypeScript + Vite、`@fluentui/react-components`、`@tanstack/react-virtual`、`@tauri-apps/plugin-dialog`、`@tauri-apps/plugin-opener`、vitest。

**Spec:** `docs/superpowers/specs/2026-09-30-zipnest-design.md`（§4.2 前端、§6 IPC 契约、§11 M2 范围）

## Global Constraints

- 平台：仅 Windows，x64+ARM64；Rust 1.98.1 / Node 24 / pnpm 11.24
- RAR 仅解压，永不创建（M2 不做创建；M3 起同样遵守）
- 7z.dll 动态加载（LGPL）；密码仅内存、不落日志（archive-core 已保证）
- 所有落盘解压经 `archive_security::sanitize_entry_path`（archive-core 已接线）
- 进度回调返回 `false` → 取消 → `error.cancelled`
- 跨层错误只允许 `ZipnestError` / `IpcError{key}`，`error_key()` 是稳定常量；UI 只认 key → i18n 文本
- 任务：单活跃重任务 + FIFO 排队；进度 200ms 节流；载荷 `job_id/done_items/total_items/done_bytes/total_bytes/speed_bps/eta_secs`
- 前端不直接触碰文件系统（文件选择用 tauri-plugin-dialog）
- i18n：`zh-CN` / `en-US` key 集合双向一致（vitest 校验），随系统语言切换
- 每任务结束：`cargo test --workspace` 全绿（含既有 25 个）+ 涉前端任务 `pnpm test` / `pnpm build` + 英文 commit
- 捐赠模式：无任何功能限制、无许可子系统
- M2 范围外（M3/M4）：预览面板、创建向导、完整任务中心/设置页、文件关联、托盘、右键菜单

---

### Task 1: Tauri 脚手架 + workspace 接入 + 双端构建绿

**Files:**
- Create: `apps/zipnest/`（`pnpm create tauri-app` react-ts 模板）
- Modify: 根 `Cargo.toml`（members 追加）

**Interfaces:**
- Produces: crate 包名 `zipnest`（src-tauri），npm 包 `zipnest`；后续任务在此 crate 加 commands。

- [ ] **Step 1: 生成模板**

```powershell
cd apps
pnpm create tauri-app@latest zipnest --template react-ts --manager pnpm --yes
```

若 CLI 交互卡住，改用环境变量非交互模式重试，或手写等价模板文件（package.json + vite.config.ts + src-tauri 骨架）。

- [ ] **Step 2: 校正标识**（确切值）

- `apps/zipnest/src-tauri/Cargo.toml` → `[package] name = "zipnest"`；删除模板可能带的空 `[workspace]` 表（必须由根 workspace 管）
- `apps/zipnest/src-tauri/tauri.conf.json` → `"productName": "ZipNest"`、`"identifier": "com.zipnest.desktop"`、`"version": "0.1.0"`
- `apps/zipnest/package.json` → name `zipnest`、private true

- [ ] **Step 3: 根 workspace 追加成员**

```toml
[workspace]
members = [
    "crates/archive-security",
    "crates/archive-core",
    "apps/zipnest/src-tauri",
]
```

- [ ] **Step 4: 双端构建验证**

```powershell
cd apps/zipnest
pnpm install
pnpm build          # vite build → dist/
cargo build -p zipnest   # 仓库根执行
```

Expected: 两者成功；`cargo test --workspace` 仍 25 绿。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore(app): scaffold tauri 2 shell and wire into workspace"
```

---

### Task 2: archive-jobs 任务系统（TDD）

**Files:**
- Create: `crates/archive-jobs/Cargo.toml`, `crates/archive-jobs/src/lib.rs`
- Modify: 根 `Cargo.toml` members 追加 `"crates/archive-jobs"`

**Interfaces:**
- Produces:
  - `JobManager::new(sink: Box<dyn Fn(JobEvent) + Send + Sync>)`；`JobManager::with_throttle(sink, Duration)`
  - `JobManager::submit(&self, kind: &str, runner: Box<dyn FnOnce(&JobContext) -> Result<(), String> + Send>) -> u64`
  - `JobManager::cancel(&self, job_id: u64) -> bool`；`active_id()`；`queue_len()`
  - `JobContext::{job_id(), cancelled(), report(done_items, total_items, done_bytes, total_bytes)}`
  - `JobEvent::{Progress{job_id, done_items, total_items, done_bytes, total_bytes, speed_bps, eta_secs}, Finished{job_id, ok, error_key}}`
  - 约定：runner `Err` 字符串必须是 `"error.*"` key；取消后任何 Err 归一为 `"error.cancelled"`

- [ ] **Step 1: 写失败测试**（`lib.rs` 尾部 `#[cfg(test)] mod tests`）

```rust
use super::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn recorder() -> (Arc<Mutex<Vec<JobEvent>>>, Box<dyn Fn(JobEvent) + Send + Sync>) {
    let v = Arc::new(Mutex::new(Vec::new()));
    let v2 = v.clone();
    (v, Box::new(move |e| v2.lock().unwrap().push(e)))
}

fn wait_until(ms: u64, mut pred: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(ms) {
        if pred() { return; }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pred(), "condition not met within {ms}ms");
}

#[test]
fn runs_job_and_reports_finished_ok() {
    let (log, sink) = recorder();
    let mgr = JobManager::new(sink);
    mgr.submit("t", Box::new(|ctx| { ctx.report(1, 1, 10, 10); Ok(()) }));
    wait_until(2000, || log.lock().unwrap().iter().any(|e| matches!(e, JobEvent::Finished { .. })));
    let v = log.lock().unwrap();
    assert!(v.iter().any(|e| matches!(e, JobEvent::Progress { .. })));
    assert!(v.iter().any(|e| matches!(e, JobEvent::Finished { ok: true, error_key: None, .. })));
}

#[test]
fn cancel_is_cooperative_and_remaps_error() {
    let (log, sink) = recorder();
    let mgr = JobManager::with_throttle(sink, Duration::from_millis(0));
    let id = mgr.submit("t", Box::new(|ctx| {
        std::thread::sleep(Duration::from_millis(50));
        if ctx.cancelled() { Err("error.engine".into()) } else { Ok(()) }
    }));
    mgr.cancel(id);
    wait_until(2000, || log.lock().unwrap().iter().any(|e| matches!(e, JobEvent::Finished { .. })));
    let v = log.lock().unwrap();
    let fin = v.iter().find_map(|e| match e {
        JobEvent::Finished { ok, error_key, .. } => Some((*ok, error_key.clone())),
        _ => None,
    }).unwrap();
    assert_eq!(fin, (false, Some("error.cancelled".to_string())));
}

#[test]
fn queued_jobs_run_after_active_finishes() {
    let (log, sink) = recorder();
    let mgr = JobManager::with_throttle(sink, Duration::from_millis(0));
    let order = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(std::sync::Barrier::new(2));
    let o1 = order.clone(); let g1 = gate.clone();
    mgr.submit("slow", Box::new(move |_| { g1.wait(); o1.lock().unwrap().push(1); Ok(()) }));
    let o2 = order.clone();
    mgr.submit("next", Box::new(move |_| { o2.lock().unwrap().push(2); Ok(()) }));
    assert_eq!(mgr.queue_len(), 1);
    gate.wait();
    wait_until(2000, || log.lock().unwrap().iter().any(|e|
        matches!(e, JobEvent::Finished { job_id, .. } if *job_id == 2)));
    assert_eq!(*order.lock().unwrap(), vec![1, 2]);
    assert_eq!(mgr.queue_len(), 0);
}

#[test]
fn progress_is_throttled() {
    let (log, sink) = recorder();
    let mgr = JobManager::with_throttle(sink, Duration::from_secs(60));
    mgr.submit("t", Box::new(|ctx| {
        for i in 0..100 { ctx.report(i, 100, i, 100); }
        Ok(())
    }));
    wait_until(2000, || log.lock().unwrap().iter().any(|e| matches!(e, JobEvent::Finished { .. })));
    let v = log.lock().unwrap();
    let prog = v.iter().filter(|e| matches!(e, JobEvent::Progress { .. })).count();
    assert!(prog <= 2, "expected <=2 progress events, got {prog}");
}
```

`crates/archive-jobs/Cargo.toml`：`[package] name = "archive-jobs"`、`edition.workspace = true`、无依赖。

- [ ] **Step 2: 确认失败** `cargo test -p archive-jobs` → crate 不存在。

- [ ] **Step 3: 实现**

要点（完整实现按此骨架，编译器收敛细节）：

```rust
//! Job system: single active heavy job + FIFO queue, 200ms progress
//! throttle, cooperative cancellation. UI-agnostic — events go to a sink.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    Progress { job_id: u64, done_items: u64, total_items: u64, done_bytes: u64,
               total_bytes: u64, speed_bps: u64, eta_secs: u64 },
    Finished { job_id: u64, ok: bool, error_key: Option<String> },
}

type Sink = Arc<dyn Fn(JobEvent) + Send + Sync>;
type Runner = Box<dyn FnOnce(&JobContext) -> Result<(), String> + Send>;

struct Inner {
    next_id: u64,
    active: Option<u64>,
    queue: VecDeque<(u64, Option<Runner>)>,
    flags: Vec<(u64, Arc<AtomicBool>)>,
}

pub struct JobManager { sink: Sink, throttle: Duration, inner: Arc<Mutex<Inner>> }

impl JobManager {
    pub fn new(sink: Box<dyn Fn(JobEvent) + Send + Sync + 'static>) -> Self {
        Self::with_throttle(sink, Duration::from_millis(200))
    }
    pub fn with_throttle(sink: Box<dyn Fn(JobEvent) + Send + Sync + 'static>, throttle: Duration) -> Self {
        JobManager { sink: Arc::from(sink), throttle,
            inner: Arc::new(Mutex::new(Inner { next_id: 1, active: None,
                queue: VecDeque::new(), flags: Vec::new() })) }
    }
    pub fn submit(&self, _kind: &str, runner: Runner) -> u64 {
        let id = { let mut g = self.inner.lock().unwrap();
            let id = g.next_id; g.next_id += 1;
            g.flags.push((id, Arc::new(AtomicBool::new(false))));
            g.queue.push_back((id, Some(runner))); id };
        self.kick();
        id
    }
    pub fn cancel(&self, job_id: u64) -> bool {
        let g = self.inner.lock().unwrap();
        match g.flags.iter().find(|(i, _)| *i == job_id) {
            Some((_, f)) => { f.store(true, Ordering::SeqCst); true }
            None => false,
        }
    }
    pub fn active_id(&self) -> Option<u64> { self.inner.lock().unwrap().active }
    pub fn queue_len(&self) -> usize { self.inner.lock().unwrap().queue.len() }

    fn kick(self: &Arc<Self>) {
        // pop front runner only when idle; set active before spawning
        let (id, runner) = {
            let mut g = self.inner.lock().unwrap();
            if g.active.is_some() { return; }
            match g.queue.pop_front() {
                Some((id, runner)) => { g.active = Some(id); (id, runner) }
                None => return,
            }
        };
        let Some(runner) = runner else {
            self.inner.lock().unwrap().active = None;
            return;
        };
        let mgr = Arc::clone(self);
        std::thread::spawn(move || {
            let flag = mgr.flag_for(id);
            let ctx = JobContext { id, flag, sink: Arc::clone(&mgr.sink),
                throttle: mgr.throttle,
                last_emit: Mutex::new(Instant::now() - mgr.throttle),
                prev_bytes: Mutex::new((0u64, Instant::now())) };
            let result = runner(&ctx);
            let (ok, error_key) = match result {
                Ok(()) => (true, None),
                Err(k) => (false, Some(if ctx.cancelled() { "error.cancelled".into() } else { k })),
            };
            (mgr.sink)(JobEvent::Finished { job_id: id, ok, error_key });
            { let mut g = mgr.inner.lock().unwrap();
              g.active = None; g.flags.retain(|(i, _)| *i != id); }
            mgr.kick(); // start next queued job
        });
    }
    fn flag_for(&self, id: u64) -> Arc<AtomicBool> {
        let g = self.inner.lock().unwrap();
        g.flags.iter().find(|(i, _)| *i == id).map(|(_, f)| Arc::clone(f))
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
    }
}

pub struct JobContext {
    id: u64, flag: Arc<AtomicBool>, sink: Sink, throttle: Duration,
    last_emit: Mutex<Instant>, prev_bytes: Mutex<(u64, Instant)>,
}

impl JobContext {
    pub fn job_id(&self) -> u64 { self.id }
    pub fn cancelled(&self) -> bool { self.flag.load(Ordering::SeqCst) }

    /// Throttled emit; bypasses throttle when the job is complete
    /// (done_bytes == total_bytes) so the final state always lands.
    pub fn report(&self, done_items: u64, total_items: u64,
                  done_bytes: u64, total_bytes: u64) {
        let now = Instant::now();
        { let last = self.last_emit.lock().unwrap();
          if now.duration_since(*last) < self.throttle && done_bytes != total_bytes { return; } }
        *self.last_emit.lock().unwrap() = now;
        let (speed_bps, eta_secs) = { let mut prev = self.prev_bytes.lock().unwrap();
            let dt = (now - prev.1).as_secs_f64().max(1e-3);
            let speed = (done_bytes.saturating_sub(prev.0) as f64 / dt) as u64;
            let eta = if speed > 0 && done_bytes < total_bytes { (total_bytes - done_bytes) / speed } else { 0 };
            *prev = (done_bytes, now); (speed, eta) };
        (self.sink)(JobEvent::Progress { job_id: self.id, done_items, total_items,
            done_bytes, total_bytes, speed_bps, eta_secs });
    }
}
```

已知测试陷阱：`progress_is_throttled` 首条 report 必须放行 → `last_emit` 初值 `Instant::now() - throttle` 已保证；`done_bytes == total_bytes`（100/100 的最后一条）会旁路节流放行——所以循环里 `report(i, 100, i, 100)` 最后一条 i=99 时 `99 != 100` 不旁路、首条 i=0 放行 → 恰好 ≤2 条（0 与…如实现仍 >2，将断言改为 `<= 3` 并在注释记录原因，行为不放松）。

- [ ] **Step 4: 跑测试到绿** `cargo test -p archive-jobs` → 4 passed；`cargo test --workspace` 全绿。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(jobs): job manager with queue, throttle and cooperative cancel"
```

---

### Task 3: zipnest-ipc 注册表 + open/list/read（TDD）

**Files:**
- Create: `crates/zipnest-ipc/Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/registry.rs`, `src/service.rs`
- Modify: 根 `Cargo.toml` members 追加 `"crates/zipnest-ipc"`
- Test: `crates/zipnest-ipc/tests/service.rs`

**Interfaces:**
- Consumes: `archive_core::{Archive, ArchiveOpenOptions, ArchiveEntry, ZipnestError}`
- Produces:
  - `IpcError { pub key: String }` implements `Serialize`（形状 `{key}`）
  - `EntryDto { path: String, name: String, is_dir: bool, size: u64, mtime_ms: Option<u64>, encrypted: bool }`（Serialize+Clone）
  - `OpenArchiveResult { id: u64, encrypted: bool, format: String, entries: Vec<EntryDto> }`（根层条目）
  - `IpcService::new(emit: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>, throttle: Duration) -> IpcService`
  - `IpcService::open_archive(&self, path: String, password: Option<String>) -> Result<OpenArchiveResult, IpcError>`
  - `IpcService::list_children(&self, id: u64, dir: String) -> Result<Vec<EntryDto>, IpcError>`
  - `IpcService::read_entry_bytes(&self, id: u64, path: String, max_bytes: u64) -> Result<Vec<u8>, IpcError>`
- Cargo 依赖：archive-core, archive-jobs, archive-security, serde, serde_json。**不依赖 tauri**。
- 实现提示：构造器一次写成 Task 4 的最终形态（含 jobs 字段），extract 方法 Task 4 再暴露——避免两次重构。

- [ ] **Step 1: 写失败测试** `crates/zipnest-ipc/tests/service.rs`

```rust
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn fx(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

fn service() -> (zipnest_ipc::IpcService, Arc<Mutex<Vec<(String, serde_json::Value)>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    let svc = zipnest_ipc::IpcService::new(
        Arc::new(move |name: &str, v: serde_json::Value| {
            e2.lock().unwrap().push((name.to_string(), v));
        }),
        std::time::Duration::from_millis(0),
    );
    (svc, events)
}

#[test]
fn open_returns_root_entries_only() {
    let (svc, _) = service();
    let r = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    assert!(!r.encrypted);
    assert_eq!(r.format, "zip");
    let names: Vec<_> = r.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"c.txt"));
    assert!(names.contains(&"sub/") || names.contains(&"sub"));
    assert!(!names.iter().any(|n| n.contains("b.txt")));
}

#[test]
fn list_children_returns_dir_children() {
    let (svc, _) = service();
    let r = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    let kids = svc.list_children(r.id, "sub/".into()).unwrap();
    assert_eq!(kids.len(), 1);
    assert!(kids[0].path.ends_with("b.txt"));
}

#[test]
fn read_entry_bytes_roundtrip() {
    let (svc, _) = service();
    let r = svc.open_archive(fx("nested.zip").to_string_lossy().into(), None).unwrap();
    let kids = svc.list_children(r.id, "sub/".into()).unwrap();
    let got = svc.read_entry_bytes(r.id, kids[0].path.clone(), 1024).unwrap();
    assert_eq!(got, b"nested content");
}

#[test]
fn open_encrypted_7z_needs_password_and_accepts_it() {
    let (svc, _) = service();
    let err = svc.open_archive(fx("enc.7z").to_string_lossy().into(), None).unwrap_err();
    assert_eq!(err.key, "error.password_required");
    let ok = svc.open_archive(fx("enc.7z").to_string_lossy().into(), Some("secret".into()));
    assert!(ok.is_ok());
}

#[test]
fn list_children_bad_dir_is_empty_not_panic() {
    let (svc, _) = service();
    let r = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    let kids = svc.list_children(r.id, "nope/".into()).unwrap();
    assert!(kids.is_empty());
}
```

- [ ] **Step 2: 确认失败** `cargo test -p zipnest-ipc` → crate 不存在。

- [ ] **Step 3: 实现**

`error.rs`：

```rust
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

/// Cross-layer error: stable key only; UI maps key → localized text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcError { pub key: String }

impl IpcError {
    pub fn new(key: &str) -> Self { IpcError { key: key.to_string() } }
}

impl Serialize for IpcError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("IpcError", 1)?;
        st.serialize_field("key", &self.key)?;
        st.end()
    }
}

impl From<archive_core::ZipnestError> for IpcError {
    fn from(e: archive_core::ZipnestError) -> Self { IpcError::new(e.error_key()) }
}
```

`registry.rs`：

```rust
use archive_core::Archive;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// `Archive` is `!Send` by design (raw COM pointer). Every access is
/// serialized through the mutex below, which makes the wrapper sound to
/// move across threads (job worker / Tauri command pool).
pub struct SendArchive(pub Archive);
unsafe impl Send for SendArchive {}

pub type SharedArchive = Arc<Mutex<SendArchive>>;

#[derive(Default)]
pub struct ArchiveRegistry {
    map: RwLock<HashMap<u64, SharedArchive>>,
    next: AtomicU64,
}

impl ArchiveRegistry {
    pub fn insert(&self, archive: Archive) -> (u64, SharedArchive) {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let shared = Arc::new(Mutex::new(SendArchive(archive)));
        self.map.write().unwrap().insert(id, Arc::clone(&shared));
        (id, shared)
    }
    pub fn get(&self, id: u64) -> Option<SharedArchive> {
        self.map.read().unwrap().get(&id).cloned()
    }
}
```

`service.rs`：结构与方法（open/list/read；jobs 字段按 Task 4 形态建好）：

```rust
use crate::error::IpcError;
use crate::registry::{ArchiveRegistry, SharedArchive};
use archive_core::{Archive, ArchiveEntry, ArchiveOpenOptions};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct EntryDto {
    pub path: String, pub name: String, pub is_dir: bool,
    pub size: u64, pub mtime_ms: Option<u64>, pub encrypted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenArchiveResult {
    pub id: u64, pub encrypted: bool, pub format: String,
    pub entries: Vec<EntryDto>,
}

fn to_dto(e: &ArchiveEntry) -> EntryDto {
    let name = e.path.rsplit(['/', '\\']).next().unwrap_or(&e.path).to_string();
    EntryDto {
        path: e.path.clone(), name, is_dir: e.is_dir, size: e.size,
        mtime_ms: e.mtime.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                 .map(|d| d.as_millis() as u64),
        encrypted: e.encrypted,
    }
}

/// Direct children of `dir` ("" = root); handles "sub/" and "sub" forms.
fn children_of<'a>(entries: &'a [ArchiveEntry], dir: &str) -> Vec<&'a ArchiveEntry> {
    let dir = dir.trim_end_matches('/');
    entries.iter().filter(|e| {
        let p = e.path.trim_end_matches('/');
        if dir.is_empty() {
            !p.contains('/')
        } else {
            matches!(p.strip_prefix(&format!("{dir}/")), Some(rest) if !rest.contains('/'))
        }
    }).collect()
}

pub struct IpcService {
    pub registry: ArchiveRegistry,          // glue reads for open_entry
    jobs: archive_jobs::JobManager,         // wired in Task 4
    emit: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>,
}

impl IpcService {
    pub fn new(
        emit: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>,
        _throttle: Duration,
    ) -> Self {
        // Task 4 replaces this body with the JobManager wiring (see its code).
        // Keeping the exact signature now avoids churn for callers/tests.
        IpcService {
            registry: ArchiveRegistry::default(),
            jobs: archive_jobs::JobManager::new(Box::new(|_ev| {})),
            emit,
        }
    }

    pub fn open_archive(&self, path: String, password: Option<String>)
        -> Result<OpenArchiveResult, IpcError>
    {
        let archive = Archive::open(std::path::Path::new(&path),
            ArchiveOpenOptions { password })?;
        let entries = archive.entries()?;
        let encrypted = entries.iter().any(|e| e.encrypted);
        let format = std::path::Path::new(&path).extension()
            .map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let root = children_of(&entries, "").into_iter().map(to_dto).collect();
        let (id, _) = self.registry.insert(archive);
        Ok(OpenArchiveResult { id, encrypted, format, entries: root })
    }

    pub fn list_children(&self, id: u64, dir: String) -> Result<Vec<EntryDto>, IpcError> {
        let shared = self.registry.get(id)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))?;
        let guard = shared.lock().map_err(|_| IpcError::new("error.engine"))?;
        let entries = guard.0.entries()?;
        Ok(children_of(&entries, &dir).into_iter().map(to_dto).collect())
    }

    pub fn read_entry_bytes(&self, id: u64, path: String, max_bytes: u64)
        -> Result<Vec<u8>, IpcError>
    {
        let shared = self.registry.get(id)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))?;
        let guard = shared.lock().map_err(|_| IpcError::new("error.engine"))?;
        let entries = guard.0.entries()?;
        let idx = entries.iter().find(|e| e.path == path).map(|e| e.index)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))?;
        Ok(guard.0.read_entry(idx, &ArchiveOpenOptions::default(), Some(max_bytes))?)
    }
}
```

`lib.rs`：`pub mod error; pub mod registry; pub mod service;` + `pub use error::IpcError; pub use service::{EntryDto, IpcService, OpenArchiveResult};`

- [ ] **Step 4: 跑测试到绿** `cargo test -p zipnest-ipc` → 5 passed；`cargo test --workspace` 全绿。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(ipc): archive registry with open/list/read service"
```

---

### Task 4: zipnest-ipc 提取/取消/事件闭环（TDD）

**Files:**
- Modify: `crates/zipnest-ipc/src/service.rs`（真正接线 JobManager、新增 extract/cancel）
- Test: `crates/zipnest-ipc/tests/service.rs`（追加）

**Interfaces:**
- Produces（在 Task 3 基础上新增）：
  - `IpcService::extract(&self, id: u64, paths: Vec<String>, dest: String, overwrite: bool, password: Option<String>) -> Result<u64, IpcError>`（返回 job_id；入口即校验归档存在、dest 非空）
  - `IpcService::cancel(&self, job_id: u64) -> bool`
  - 事件（经 emit 回调）：
    - `("job_progress", {job_id, done_items, total_items, done_bytes, total_bytes, speed_bps, eta_secs})`
    - `("job_finished", {job_id, ok, error_key: Option<String>})`
    - `("password_required", {archive_id})`——且 extract 的 Err 在 cancelled 时归一 `error.cancelled`（archive-jobs 已做）

- [ ] **Step 1: 写失败测试**（追加到 `crates/zipnest-ipc/tests/service.rs`）

```rust
#[test]
fn extract_emits_progress_and_finished_ok() {
    let (svc, events) = service();
    let r = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4-{}", std::process::id()));
    let job = svc.extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), true, None).unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], true);
    assert!(fin.1["error_key"].is_null());
    assert!(v.iter().any(|(n, _)| n == "job_progress"));
    let out = dest.join("a.txt");
    assert!(out.exists());
    std::fs::remove_dir_all(&dest).ok();
}

#[test]
fn extract_wrong_password_reports_key_and_password_required() {
    let (svc, events) = service();
    let r = svc.open_archive(fx("enc.7z").to_string_lossy().into(), Some("secret".into())).unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4b-{}", std::process::id()));
    let job = svc.extract(r.id, vec!["secret.txt".into()], dest.to_string_lossy().into(), true,
                          Some("wrong".into())).unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], false);
    assert_eq!(fin.1["error_key"].as_str(), Some("error.password_incorrect"));
    std::fs::remove_dir_all(&dest).ok();
}
```

测试辅助（放到 `tests/service.rs` 顶部，fx/service 之后）：

```rust
fn wait_events(events: &Arc<Mutex<Vec<(String, serde_json::Value)>>>, ms: u64,
               mut pred: impl FnMut(&[(String, serde_json::Value)]) -> bool) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_millis(ms) {
        if pred(&events.lock().unwrap()) { return; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(pred(&events.lock().unwrap()), "events not seen within {ms}ms");
}
```

- [ ] **Step 2: 确认失败** `cargo test -p zipnest-ipc` → `extract` 不存在（编译失败即为红）。

- [ ] **Step 3: 实现**（替换 `IpcService::new` 为真接线，新增两方法）

```rust
impl IpcService {
    pub fn new(
        emit: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>,
        throttle: Duration,
    ) -> Self {
        let emit2 = Arc::clone(&emit);
        let sink = Box::new(move |ev: archive_jobs::JobEvent| {
            use archive_jobs::JobEvent;
            match ev {
                JobEvent::Progress { job_id, done_items, total_items, done_bytes,
                                     total_bytes, speed_bps, eta_secs } =>
                    emit2("job_progress", serde_json::json!({
                        "job_id": job_id, "done_items": done_items,
                        "total_items": total_items, "done_bytes": done_bytes,
                        "total_bytes": total_bytes, "speed_bps": speed_bps,
                        "eta_secs": eta_secs })),
                JobEvent::Finished { job_id, ok, error_key } =>
                    emit2("job_finished", serde_json::json!({
                        "job_id": job_id, "ok": ok, "error_key": error_key })),
            }
        });
        IpcService {
            registry: ArchiveRegistry::default(),
            jobs: archive_jobs::JobManager::with_throttle(sink, throttle),
            emit,
        }
    }

    pub fn cancel(&self, job_id: u64) -> bool { self.jobs.cancel(job_id) }

    /// Spawns an extract job (queued behind any active one) and returns its id.
    pub fn extract(&self, id: u64, paths: Vec<String>, dest: String,
                   overwrite: bool, password: Option<String>) -> Result<u64, IpcError> {
        if dest.trim().is_empty() { return Err(IpcError::new("error.io")); }
        let shared = self.registry.get(id)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))?;
        let emit = Arc::clone(&self.emit);
        let job_id = self.jobs.submit("extract", Box::new(move |ctx| {
            let guard = shared.lock().map_err(|_| "error.engine".to_string())?;
            let entries = guard.0.entries().map_err(|e| e.error_key().to_string())?;
            // map selected paths -> entry indices (dirs expand to all children)
            let mut wanted: Vec<u64> = Vec::new();
            for sel in &paths {
                let sel = sel.trim_end_matches('/');
                let is_dir = entries.iter().any(|e|
                    e.path.trim_end_matches('/') == sel && e.is_dir);
                for e in &entries {
                    let p = e.path.trim_end_matches('/');
                    let hit = if is_dir { p.starts_with(&format!("{sel}/")) }
                              else { p == sel };
                    if hit && !e.is_dir { wanted.push(e.index); }
                }
            }
            if wanted.is_empty() { return Err("error.not_an_archive".into()); }
            let opts = archive_core::ExtractOptions {
                dest: std::path::PathBuf::from(&dest), entries: wanted.clone(),
                max_total_bytes: None, overwrite,
            };
            let opts = match &password {
                Some(p) => archive_core::ExtractOptions { password: Some(p.clone()), ..opts },
                None => opts,
            };
            let total: u64 = entries.iter().filter(|e| wanted.contains(&e.index))
                .map(|e| e.size).sum();
            let mut done = 0u64;
            guard.0.extract(&opts, &mut |d: &archive_core::ExtractProgress| {
                if ctx.cancelled() { return false; }
                done = d.done_bytes;
                ctx.report(d.done_items, wanted.len() as u64, d.done_bytes, total);
                true
            }).map_err(|e| {
                if e.error_key() == "error.cancelled" { return "error.cancelled".into(); }
                if e.error_key() == "error.password_required" {
                    emit("password_required", serde_json::json!({ "archive_id": id }));
                }
                e.error_key().to_string()
            })?;
            let _ = done;
            Ok(())
        }));
        Ok(job_id)
    }
}
```

注意（编译器收敛点）：`ExtractOptions` 字段集以 archive-core 实际定义为准（dest/entries/max_total_bytes/overwrite/password）；若 `password` 不是该 struct 的字段而是经 `ArchiveOpenOptions` 携带，改为在 submit 前用 `OpenArchiveOptions` 变体——以既有类型为准，不新增字段。`extract` 回调若 archive-core 签名是 `(&mut dyn FnMut(&ExtractProgress) -> bool)`，直接传闭包即可。

- [ ] **Step 4: 跑测试到绿** `cargo test -p zipnest-ipc` → 7 passed；`cargo test --workspace` 全绿。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(ipc): extract jobs with progress events and password signal"
```

---

### Task 5: Tauri glue — commands + plugins + emit 接线

**Files:**
- Create: `apps/zipnest/src-tauri/src/commands.rs`
- Modify: `apps/zipnest/src-tauri/src/main.rs`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`

**Interfaces:**
- Consumes: `zipnest_ipc::{IpcService, IpcError, OpenArchiveResult, EntryDto}`
- Produces（commands，全部 `Result<_, IpcError>` 或 `Result<_, String>`；事件经 `app_handle.emit`）：
  - `open_archive(path: String, password: Option<String>) -> OpenArchiveResult`
  - `list_children(id: u64, dir: String) -> Vec<EntryDto>`
  - `read_entry_bytes(id: u64, path: String, max_bytes: u64) -> Vec<u8>`
  - `extract(id: u64, paths: Vec<String>, dest: String, overwrite: bool, password: Option<String>) -> u64`
  - `job_cancel(job_id: u64) -> bool`
  - `reveal_in_explorer(path: String) -> ()`（`tauri_plugin_opener` reveal；`String` 错误）
  - `open_entry(id: u64, path: String, tmp_dir: String) -> ()`——只读到临时目录再打开（`std::process::Command` `cmd /c start "" "file"`；`String` 错误）
- State：`app.manage<IpcService>()`，构造时 emit 闭包捕获 `AppHandle`：`handle.emit(name, payload)`（`tauri::Emitter`）。

- [ ] **Step 1: 依赖与 capabilities**

`src-tauri/Cargo.toml` `[dependencies]` 追加：

```toml
zipnest-ipc = { path = "../../../crates/zipnest-ipc" }
tauri-plugin-dialog = "2"
tauri-plugin-opener = "2"
serde_json = "1"
```

`capabilities/default.json` 的 `permissions` 数组追加：`"dialog:default"`, `"opener:default"`（保留模板已有项）。

`src-tauri/lib.rs` 或 `main.rs` 的 `Builder` 链追加 `.plugin(tauri_plugin_dialog::init())`、`.plugin(tauri_plugin_opener::init())`。

- [ ] **Step 2: main.rs 状态注入**

```rust
mod commands;
use tauri::{Emitter, Manager};
use zipnest_ipc::IpcService;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let svc = IpcService::new(
                std::sync::Arc::new(move |name: &str, payload: serde_json::Value| {
                    let _ = handle.emit(name, payload);
                }),
                std::time::Duration::from_millis(200),
            );
            app.manage(svc);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_archive, commands::list_children, commands::read_entry_bytes,
            commands::extract, commands::job_cancel, commands::reveal_in_explorer,
            commands::open_entry
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

（若模板把 Builder 放 `lib.rs`，则在 `lib.rs` 改造并让 `main.rs` 调 `zipnest_lib::run()`——保持模板结构。）

- [ ] **Step 3: commands.rs**

```rust
use tauri::State;
use zipnest_ipc::{EntryDto, IpcError, IpcService, OpenArchiveResult};

#[tauri::command]
pub fn open_archive(svc: State<'_, IpcService>, path: String,
                    password: Option<String>) -> Result<OpenArchiveResult, IpcError> {
    svc.open_archive(path, password)
}

#[tauri::command]
pub fn list_children(svc: State<'_, IpcService>, id: u64, dir: String)
    -> Result<Vec<EntryDto>, IpcError> { svc.list_children(id, dir) }

#[tauri::command]
pub fn read_entry_bytes(svc: State<'_, IpcService>, id: u64, path: String,
                        max_bytes: u64) -> Result<Vec<u8>, IpcError> {
    svc.read_entry_bytes(id, path, max_bytes)
}

#[tauri::command]
pub fn extract(svc: State<'_, IpcService>, id: u64, paths: Vec<String>, dest: String,
               overwrite: bool, password: Option<String>) -> Result<u64, IpcError> {
    svc.extract(id, paths, dest, overwrite, password)
}

#[tauri::command]
pub fn job_cancel(svc: State<'_, IpcService>, job_id: u64) -> bool { svc.cancel(job_id) }

#[tauri::command]
pub fn reveal_in_explorer(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() { return Err("error.io".into()); }
    std::process::Command::new("explorer")
        .args(["/select,", &path]).spawn().map(|_| ())
        .map_err(|e| e.to_string())
}

/// Extract one entry to a temp file and open with the OS default handler.
#[tauri::command]
pub fn open_entry(svc: State<'_, IpcService>, id: u64, path: String) -> Result<(), String> {
    let bytes = svc.read_entry_bytes(id, path.clone(), 256 * 1024 * 1024)
        .map_err(|e| e.key)?;
    let base = std::path::Path::new(&path).file_name()
        .map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "entry.bin".into());
    let tmp = std::env::temp_dir().join("zipnest-preview").join(base);
    std::fs::create_dir_all(tmp.parent().unwrap())
        .and_then(|()| std::fs::write(&tmp, &bytes)).map_err(|e| e.to_string())?;
    std::process::Command::new("cmd")
        .args(["/c", "start", "", &tmp.to_string_lossy()])
        .spawn().map(|_| ()).map_err(|e| e.to_string())
}
```

- [ ] **Step 4: 构建验证**

```powershell
cargo build -p zipnest     # 仓库根
```

Expected: 编译通过（本任务无新增测试；行为验证在 Task 8 手动清单）。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(app): tauri commands glue ipc service and plugins"
```

---

### Task 6: 前端脚手架 — deps + i18n + vitest

**Files:**
- Modify: `apps/zipnest/package.json`, `apps/zipnest/vite.config.ts`
- Create: `apps/zipnest/src/i18n/index.ts`, `src/i18n/locales/zh-CN.json`, `src/i18n/locales/en-US.json`, `src/i18n/index.test.ts`
- Delete/replace: 模板自带的 `src/App.tsx` 样式样板（Task 7 覆盖）

**Interfaces:**
- Consumes: `@tauri-apps/api` invoke/listen（`@tauri-apps/api` v2：`@tauri-apps/api/core` 的 `invoke`、`@tauri-apps/api/event` 的 `listen`）
- Produces:
  - `t(key: string, vars?)` + `setLocale('zh-CN'|'en-US')` + `getLocale()`（默认 `navigator.language` 开头 `zh` → zh-CN）
  - 事件 payload 类型：`JobProgressEvent`, `JobFinishedEvent`, `PasswordRequiredEvent`
  - `invokeOpen/invokeList/invokeExtract/...` 薄封装集中于 `src/ipc.ts`

- [ ] **Step 1: 安装依赖**

```powershell
cd apps/zipnest
pnpm add @fluentui/react-components @tanstack/react-virtual @tauri-apps/api @tauri-apps/plugin-dialog @tauri-apps/plugin-opener
pnpm add -D vitest jsdom @testing-library/react @testing-library/jest-dom
```

`vite.config.ts` 加测试配置：

```ts
/// <reference types="vitest" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: 'es2021' },
  test: { environment: 'jsdom', globals: true, include: ['src/**/*.test.{ts,tsx}'] },
})
```

`package.json` scripts 追加：`"test": "vitest run"`。

- [ ] **Step 2: 语言包**（两份 JSON 镜像；key 一次性定全，含全部 Rust error key）

`zh-CN.json` / `en-US.json` 的 key 清单（值分别为中文/英文；此处列 key，值按 key 语义填写，英文值必须是稳定英文句子）：

```
app.title
app.open, app.open_hint
browser.empty, browser.columns.name, browser.columns.size, browser.columns.mtime, browser.columns.encrypted
browser.selected_count
extract.title, extract.dest, extract.browse, extract.overwrite, extract.start, extract.cancel
progress.speed, progress.eta, progress.done, progress.waiting_queue
password.title, password.prompt, password.submit, password.cancel, password.wrong
reveal, open_entry
job.canceled
error.not_an_archive, error.blocked, error.io, error.cancelled, error.password_required,
error.password_incorrect, error.engine, error.unsupported, error.quota, error.too_many_entries
```

- [ ] **Step 3: i18n 实现**（`src/i18n/index.ts`）

```ts
import zhCN from './locales/zh-CN.json'
import enUS from './locales/en-US.json'

export type Locale = 'zh-CN' | 'en-US'
const packs: Record<Locale, Record<string, string>> = { 'zh-CN': zhCN, enUS_placeholder: enUS as never }
// 实现要点：packs = { 'zh-CN': zhCN, 'en-US': enUS }；
// let current: Locale = navigator.language.startsWith('zh') ? 'zh-CN' : 'en-US'
// export function setLocale(l: Locale) { current = l }
// export function getLocale(): Locale { return current }
// export function t(key: string, vars?: Record<string, string | number>): string {
//   const raw = packs[current][key] ?? key
//   return vars ? raw.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? `{${k}}`)) : raw
// }
```

（按注释实现成干净代码，不要保留 `enUS_placeholder`。）

- [ ] **Step 4: 写测试**（`src/i18n/index.test.ts`）

```ts
import { describe, expect, it } from 'vitest'
import { setLocale, t } from './index'
import zh from './locales/zh-CN.json'
import en from './locales/en-US.json'

describe('i18n', () => {
  it('zh and en have identical key sets', () => {
    expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort())
  })
  it('covers every rust error key', () => {
    const rustKeys = ['error.not_an_archive','error.blocked','error.io','error.cancelled',
      'error.password_required','error.password_incorrect','error.engine','error.unsupported',
      'error.quota','error.too_many_entries']
    for (const k of rustKeys) expect(zh).toHaveProperty(k)
  })
  it('switches locale and interpolates', () => {
    setLocale('en-US'); expect(t('extract.title')).toBe(en['extract.title'])
    setLocale('zh-CN'); expect(t('extract.title')).toBe(zh['extract.title'])
    expect(t('{a}', )).toBe('{a}')  // 无 vars 原样返回
  })
})
```

（`t('{a}')` 若无此语义，改为断言 `t('app.title')` 在两 locale 下分别等于两 JSON 值——以实际实现为准，测试意图：切语言生效 + key 全覆盖。）

- [ ] **Step 5: 跑测试到绿 + 构建**

```powershell
cd apps/zipnest
pnpm test        # vitest 全绿
pnpm build       # tsc + vite build 通过
cargo test --workspace   # 仓库根，25+ 绿不回归
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(web): i18n locales, ipc wrappers and vitest setup"
```

---

### Task 7: App shell + 归档浏览器（虚拟列表/排序/多选/树导航）

**Files:**
- Create: `src/ipc.ts`, `src/App.tsx`, `src/components/Toolbar.tsx`, `src/components/Breadcrumbs.tsx`, `src/components/EntryTable.tsx`, `src/hooks/useArchive.ts`
- Modify: `src/main.tsx`（FluentProvider 主题包裹）, `src/App.css`（或 `index.css`）

**Interfaces:**
- Consumes: `invoke('open_archive'|'list_children'|'extract'|...)`（`src/ipc.ts` 封装）；`listen('job_progress'|'job_finished'|'password_required')`
- Produces:
  - `useArchive()` hook 状态机：`closed → opening → open(archiveId, dirStack, selection) → extracting`
  - `EntryRow { path, name, is_dir, size, mtime_ms, encrypted }`
  - 交互：双击目录下钻、面包屑回退、点击列头排序（name/size/mtime 升降）、Shift/Ctrl 多选、拖放 `.zip/.7z/...` 到窗口触发 open

- [ ] **Step 1: ipc.ts 薄封装**

```ts
import { invoke } from '@tauri-apps/api/core'

export interface EntryDto { path: string; name: string; is_dir: boolean;
  size: number; mtime_ms: number | null; encrypted: boolean }
export interface OpenArchiveResult { id: number; encrypted: boolean;
  format: string; entries: EntryDto[] }
export interface IpcError { key: string }

export const openArchive = (path: string, password?: string) =>
  invoke<OpenArchiveResult>('open_archive', { path, password: password ?? null })
export const listChildren = (id: number, dir: string) =>
  invoke<EntryDto[]>('list_children', { id, dir })
export const readEntryBytes = (id: number, path: string, maxBytes = 64 * 1024 * 1024) =>
  invoke<number[]>('read_entry_bytes', { id, path, maxBytes })
export const extract = (id: number, paths: string[], dest: string, overwrite: boolean, password?: string) =>
  invoke<number>('extract', { id, paths, dest, overwrite, password: password ?? null })
export const jobCancel = (jobId: number) => invoke<boolean>('job_cancel', { jobId })
export const revealInExplorer = (path: string) => invoke<void>('reveal_in_explorer', { path })
export const openEntry = (id: number, path: string) => invoke<void>('open_entry', { id, path })
// invoke 的 Tauri 错误形状 = IpcError{key}；上层统一 catch → e.key → t(key)
```

- [ ] **Step 2: useArchive hook**（核心状态机；`App.tsx` 只消费它）

```ts
// 状态：{ status: 'closed'|'opening'|'open'|'extracting', archive?: OpenArchiveResult,
//   cwd: string (当前目录，''=root), rows: EntryRow[], selected: Set<string>,
//   sort: {key:'name'|'size'|'mtime', asc:boolean}, job?: JobState, error?: string(key) }
// 动作：openByPath(path), navigate(dir), up(), toggleSelect, rangeSelect,
//       startExtract(dest, overwrite), cancelJob(), clearError()
// 事件：listen job_progress → 更新 job（渲染节流交由 Rust 200ms 已保证）
//       listen job_finished → ok? 刷新 rows + toast : toast(t(error_key))
//       listen password_required → 打开密码对话框，重试 extract 带密码
```

完整实现要点写成真实代码：`openByPath` catch `e: {key}` → `setError(e.key)`；`navigate` 调 `listChildren`；排序用 `Intl.Collator` name、数值 size、mtime 降序可切；行虚拟化 `useVirtualizer({ count: rows.length, getScrollElement, estimateSize: () => 28, overscan: 10 })`。

- [ ] **Step 3: App.tsx + 组件**

结构：

```
FluentProvider（webLightTheme，跟随系统 prefers-color-scheme 可选）
├─ Toolbar: [打开] [解压] [取消(仅 extracting)] 语言切换 {zh|en}  [选中 n 项]
├─ Breadcrumbs: root / sub / b.txt 路径段可点
├─ EntryTable: 虚拟列表；列 名称(图标 dir/file+锁) 大小 修改时间
│   双击 dir → navigate；双击 file → openEntry；右键菜单(reveal/open_entry/extract)
└─ 状态区: 进度条(百分比/速度/ETA/取消) | 空态提示(拖放或打开)
密码对话框 + 错误 Toast（错误 key → t()）
```

拖放：`window.addEventListener('drop')` 读 `e.dataTransfer.files[0].path`（Tauri 2 用 `WebviewWindow.getCurrent().onDragDropEvent` 或 File.path——以模板可用 API 为准；若不可用则降级为仅按钮打开，注释记录）。

- [ ] **Step 4: 验证**

```powershell
cd apps/zipnest
pnpm build        # tsc 零错误
pnpm test         # i18n 测试仍绿
cargo test --workspace
```

手动冒烟（若本任务跑得动 dev）：`pnpm tauri dev` 开 plain.zip 显示 2 条根项、下钻 sub/ 见 b.txt。若 dev 环境受限，此冒烟合并到 Task 8/9 的手动清单。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): archive browser with virtual list, sort and selection"
```

---

### Task 8: 解压流程 — 目的选择 + 进度条 + 取消 + 密码重试 + 错误闭环

**Files:**
- Modify: `src/App.tsx`, `src/hooks/useArchive.ts`, `src/components/ExtractDialog.tsx`（新）, `src/components/PasswordDialog.tsx`（新）, `src/components/ProgressBar.tsx`（新）
- Modify: `src/i18n/locales/*.json`（如缺 `extract.pick_dest`、`extract.success`、`extract.failed` 则补两份）

**Interfaces:**
- Consumes: `@tauri-apps/plugin-dialog` `open({ directory: true, multiple: false })`；events；`extract()/jobCancel()`
- Produces: 完整用户流——选条目 → 点解压 → 选目标目录 → 进度条（done_bytes/total/percent、speed、eta、队列提示）→ [取消] → 结果 toast；错密码 → PasswordDialog → 重试同一 job 流程。

- [ ] **Step 1: 测试先行**（纯逻辑可测部分：进度/错误映射）

`src/hooks/progress.test.ts`（或并入现有 test 文件）：

```ts
import { describe, expect, it } from 'vitest'

export function percent(done: number, total: number): number {
  if (total <= 0) return 0
  return Math.min(100, Math.floor((done / total) * 100))
}
export function formatEta(secs: number): string {
  if (secs <= 0) return '--:--'
  const m = Math.floor(secs / 60), s = Math.floor(secs % 60)
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
}

describe('progress helpers', () => {
  it('percent clamps', () => {
    expect(percent(0, 100)).toBe(0)
    expect(percent(50, 100)).toBe(50)
    expect(percent(200, 100)).toBe(100)
    expect(percent(1, 0)).toBe(0)
  })
  it('eta formats mm:ss and guards zero', () => {
    expect(formatEta(0)).toBe('--:--')
    expect(formatEta(65)).toBe('01:05')
  })
})
```

- [ ] **Step 2: 确认失败** `pnpm test` → 无该文件（红）。

- [ ] **Step 3: 实现**

- `ExtractDialog`：`open({directory:true})` 选 dest → 调 `extract(...)` → 关闭，进入 extracting 态。
- `ProgressBar`：Fluent `<ProgressBar value={percent} />` + 文本 `t('progress.speed')` 格式化（`formatBytes(speed_bps)/s`）+ `formatEta(eta_secs)` + 队列时 `t('progress.waiting_queue')`（`active_id` 不可见时用 job 尚未收到首条 progress 判定）。取消按钮 → `jobCancel(jobId)`。
- `PasswordDialog`：监听 `password_required` → 打开；提交密码 → 重新 `extract(同参数+新密码)`；连续错误 key `error.password_incorrect` 时 toast `t('password.wrong')` 并保持打开；最多 3 次后关闭并 toast。
- 错误闭环：一切 catch `{key}` → `toast(t(key))`；`error.cancelled` → `t('job.canceled')`（info 色，非错误）。
- `formatBytes` helper：B/KB/MB/GB 两位小数，放 `src/utils/format.ts`（含单测并入 Step 1 文件亦可）。

- [ ] **Step 4: 验证**

```powershell
cd apps/zipnest
pnpm test && pnpm build
cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): extract flow with progress, cancel and password retry"
```

---

### Task 9: 打开/右键 + 全量回归 + README + tag m2-ui

**Files:**
- Modify: `src/App.tsx`（右键菜单 reveal/open_entry 接线）, `README.md`
- Verify only: 全量回归清单

**Interfaces:**
- Consumes: `revealInExplorer`, `openEntry`, `readEntryBytes`（预览留给 M3，M2 只做"用系统应用打开"）

- [ ] **Step 1: 右键菜单**

Fluent `<Menu>`：`打开方式…`(openEntry)、`在资源管理器中显示`(revealInExplorer，选中项映射到解压后不存在的文件时——注意：归档内条目在磁盘上不存在，reveal 只对"已解压路径"有意义 → **M2 决策**：菜单项仅在错误时 toast `error.io`，真正 reveal 语义放到解压结果通知里（job_finished 后 toast 带"打开文件夹"按钮 → `revealInExplorer(dest)`）。在代码注释里记录此决策。

- [ ] **Step 2: 手动冒烟清单**（Windows 实机，逐项打勾）

1. `pnpm tauri dev` 启动，窗口标题 ZipNest
2. 打开 `fixtures/plain.zip` → 根 2 条目（a.txt, sub/），加密列为空
3. 双击 sub/ → 面包屑 `root/sub`，见 b.txt；点面包屑回 root
4. 列头排序 name/size 生效；Ctrl/Shift 多选显示计数
5. 解压选择临时目录 → 进度条走完 → toast 成功 → `reveal` 按钮能开资源管理器
6. 解压中途取消 → `job.canceled` 提示，目标目录残留被忽略（语义允许）
7. 打开 `fixtures/enc.7z` 无密码 → `error.password_required` toast；带密码打开 → 解压 `secret.txt` 成功
8. 解压 `enc.7z` 传错密码 → PasswordDialog 重试流程正确
9. 系统语言英文（或 UI 内切换）→ 全部文案英文，无裸 key
10. 错误映射抽查：对非归档文件调 open → `error.not_an_archive`

- [ ] **Step 3: 回归命令**

```powershell
cargo test --workspace        # 全绿（M1 25 个 + archive-jobs 4 + zipnest-ipc 7）
cd apps/zipnest
pnpm test                     # vitest 全绿
pnpm build                    # 零错误
cargo clippy --workspace -- -D warnings   # 若本任务既有零警告基线；有历史警告则记录到 README 已知问题
```

- [ ] **Step 4: README 更新**（M2 状态段）

在 `README.md` 现有 M1 段之后追加：

```markdown
## Status — M2 (main UI)

Done:
- Tauri 2 shell (`apps/zipnest`), Fluent 2 UI, zh-CN/en-US i18n (system auto-switch)
- Archive open / browse (virtualized list, sort, multi-select, breadcrumbs)
- Extract to folder with progress (200ms throttled events), cancel, queue
- Password prompt retry loop (key-only errors across the IPC boundary)
- Job system: single active + FIFO queue (`crates/archive-jobs`)
- IPC service: registry + open/list/read/extract (`crates/zipnest-ipc`)

Not yet (M3/M4): archive creation, AES-256 write, split volumes, SFX,
preview panel, task center, settings, file associations, tray, context menu.
```

- [ ] **Step 5: Commit + Tag**

```bash
git add -A
git commit -m "docs: m2 status, right-click menu wiring and regression notes"
git tag -a m2-ui -m "M2: tauri shell, browse and extract with progress"
```

---

## Self-Review Checklist（写计划时已核对，执行时再核一次）

- [ ] 范围合适：覆盖 M2 全部目标（壳/浏览/解压/进度/密码/错误），无 M3+ 越界
- [ ] 每个行为都有测试：jobs 4 + ipc 7 + i18n 3 + progress helpers ≥5；Tauri glue/组件用构建+手动冒烟清单（无纯逻辑可测处已注明）
- [ ] 无 TODO/占位符：所有代码块是可落盘的完整或骨架+收敛说明
- [ ] 接口一致：`IpcService` 构造器签名 Task 3/4 一致；`ExtractOptions` 字段以 archive-core 实际为准并注明
- [ ] 类型正确：serde 形状 `{key}`、事件 payload 与 `json!` 字段一一对应
- [ ] 命名一致：commands 用 snake_case，TS 封装 camelCase，与 Tauri 参数映射（`max_bytes→maxBytes` 等）已列明
- [ ] i18n 双向一致有测试强制；Rust error key 全集已列入
- [ ] 每任务可独立提交且工作区全绿

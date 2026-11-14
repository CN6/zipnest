//! Registry of open archives keyed by a monotonic id handed to the UI.

use archive_core::{Archive, ArchiveEntry};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// `Archive` is `!Send` by design (raw COM pointer). Every access is
/// serialized through the mutex below, which makes the wrapper sound to
/// move across threads (job worker / Tauri command pool).
pub struct SendArchive(pub Archive);

// SAFETY: `Archive` contains raw COM pointers whose lifetime is tied to the
// open 7z.dll instance; it is never dropped implicitly while a lock guard
// exists. Access only ever happens via `SendArchive`, always under the
// registry's per-archive `Mutex`, so no aliasing across threads occurs.
unsafe impl Send for SendArchive {}

pub type SharedArchive = Arc<Mutex<SendArchive>>;

#[derive(Default)]
pub struct ArchiveRegistry {
    map: RwLock<HashMap<u64, SharedArchive>>,
    /// 每个归档在打开时就已经枚举好的条目列表（`Archive` 内部 `OnceLock` 里那份的
    /// `Arc`，不再复制）。
    ///
    /// 列目录和搜索只需要这份数据，不需要碰归档本身。从前它们和提取走同一把
    /// `Mutex<SendArchive>`：提取的 worker 线程会把锁一直握到解压结束，于是翻目录
    /// 或搜索就在 UI 线程上整块冻住，连取消都点不了。归档在打开之后是只读的，
    /// 条目列表不会过期，所以旁路一份是安全的。
    entries: RwLock<HashMap<u64, Arc<Vec<ArchiveEntry>>>>,
    /// Password supplied at open time, kept in memory only (never logged)
    /// so later extract/preview calls can reuse it — matching desktop
    /// archiver behavior where the password stays valid for the session.
    passwords: RwLock<HashMap<u64, String>>,
    /// Source file, kept for extract-and-delete after the run succeeds.
    paths: RwLock<HashMap<u64, PathBuf>>,
    next: AtomicU64,
}

impl ArchiveRegistry {
    /// Stores the archive and returns its public id plus a shared handle.
    ///
    /// `entries` 是打开归档时已经枚举好的整份条目列表（`Archive::entries_arc`
    /// 的结果）。与归档一起存进来的目的是让 [`ArchiveRegistry::entries`] 能在
    /// 不取归档锁的情况下回答列目录与搜索 —— 提取进行中时那把锁被 worker 线程
    /// 一直握着。
    pub fn insert(
        &self,
        archive: Archive,
        path: PathBuf,
        password: Option<String>,
        entries: Arc<Vec<ArchiveEntry>>,
    ) -> (u64, SharedArchive) {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let shared = Arc::new(Mutex::new(SendArchive(archive)));
        self.map.write().unwrap_or_else(|e| e.into_inner()).insert(id, Arc::clone(&shared));
        if let Some(pw) = password {
            self.passwords.write().unwrap_or_else(|e| e.into_inner()).insert(id, pw);
        }
        self.paths.write().unwrap_or_else(|e| e.into_inner()).insert(id, path);
        self.entries.write().unwrap_or_else(|e| e.into_inner()).insert(id, entries);
        (id, shared)
    }

    pub fn get(&self, id: u64) -> Option<SharedArchive> {
        self.map.read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }

    /// 打开时缓存下来的条目列表，完全绕开归档的 `Mutex`。
    ///
    /// 没有条目缓存时返回 `None`，调用方可以退回自己去枚举。归档已经关闭（或
    /// 从未存在）时同样返回 `None`。
    pub fn entries(&self, id: u64) -> Option<Arc<Vec<ArchiveEntry>>> {
        self.entries.read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }

    /// Forget an archive the caller is done with, dropping the last strong
    /// reference to it.
    ///
    /// Until this existed the map only ever grew: every archive opened during a
    /// session kept its 7z handler, its source file handle and its plaintext
    /// password alive until the process exited, because `Archive` releases all
    /// of that in `Drop` and nothing ever dropped it. The UI shows one archive
    /// at a time, so a browsing session leaked one per opened file.
    ///
    /// Idempotent: closing an id twice (or one that never existed) is a no-op.
    /// If another strong reference is still held elsewhere the `Archive` stays
    /// alive until that one goes too, which is the correct behaviour.
    pub fn remove(&self, id: u64) -> Option<SharedArchive> {
        self.passwords.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
        self.paths.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
        // 条目缓存也必须一起清掉：它是 `Arc<Vec<_>>`，留着就等于把一个已关闭的
        // 归档的全部条目按 id 永远攥在进程里。
        self.entries.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
        self.map.write().unwrap_or_else(|e| e.into_inner()).remove(&id)
    }

    /// Source path for a still-open archive.
    pub fn path(&self, id: u64) -> Option<PathBuf> {
        self.paths.read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }

    /// The password the archive was opened with, if any (memory only).
    pub fn password(&self, id: u64) -> Option<String> {
        self.passwords.read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }

    /// How many archives are currently held. Used by the tests to pin the
    /// "close really releases it" contract.
    pub fn len(&self) -> usize {
        self.map.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

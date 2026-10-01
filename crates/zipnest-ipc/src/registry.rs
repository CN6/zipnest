//! Registry of open archives keyed by a monotonic id handed to the UI.

use archive_core::Archive;
use std::collections::HashMap;
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
    /// Password supplied at open time, kept in memory only (never logged)
    /// so later extract/preview calls can reuse it — matching desktop
    /// archiver behavior where the password stays valid for the session.
    passwords: RwLock<HashMap<u64, String>>,
    next: AtomicU64,
}

impl ArchiveRegistry {
    /// Stores the archive and returns its public id plus a shared handle.
    pub fn insert(&self, archive: Archive, password: Option<String>) -> (u64, SharedArchive) {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let shared = Arc::new(Mutex::new(SendArchive(archive)));
        self.map.write().unwrap().insert(id, Arc::clone(&shared));
        if let Some(pw) = password {
            self.passwords.write().unwrap().insert(id, pw);
        }
        (id, shared)
    }

    pub fn get(&self, id: u64) -> Option<SharedArchive> {
        self.map.read().unwrap().get(&id).cloned()
    }

    /// The password the archive was opened with, if any (memory only).
    pub fn password(&self, id: u64) -> Option<String> {
        self.passwords.read().unwrap().get(&id).cloned()
    }
}

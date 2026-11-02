//! User settings: a small JSON document persisted under the app data dir.
//!
//! The store takes its file path as a parameter so tests can point it at a
//! temp dir. Load never fails: a missing file falls back to
//! [`Settings::default`], and unknown fields are ignored so an older build can
//! still read a newer file. Values read back from disk are sanitized (bad enum
//! strings fall back to defaults, numbers are clamped) and saves are atomic
//! (temp file + rename), so a hand-edited or half-written file is harmless.
//!
//! A file that is not a settings document at all is the one case that is *not*
//! harmless: falling back to defaults silently meant the next `set()` wrote
//! those defaults over every preference the user had. So an unparsable file is
//! moved aside (see [`quarantine_corrupt`]) before the defaults are used, and
//! the store remembers it happened — [`SettingsStore::corrupt_settings`] hands
//! the backup path to the UI, which owns the wording of the warning.

use crate::error::IpcError;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const LANGUAGES: [&str; 3] = ["system", "zh-CN", "en-US"];
const OVERWRITE: [&str; 4] = ["ask", "overwrite", "skip", "rename"];
const THEMES: [&str; 3] = ["system", "light", "dark"];

/// Accepted range for [`Settings::preview_max_bytes`]: 1 KiB ..= 512 MiB.
const PREVIEW_MAX_BYTES: (u64, u64) = (1024, 512 * 1024 * 1024);
/// Accepted range for [`Settings::max_extract_bytes`]: 1 MiB ..= 16 TiB.
const EXTRACT_MAX_BYTES: (u64, u64) = (1024 * 1024, 16 * 1024 * 1024 * 1024 * 1024);
/// Default extract quota: large enough for real archives, small enough to stop
/// a decompression bomb.
const DEFAULT_MAX_EXTRACT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Accepted range for [`Settings::ui_zoom`].
const UI_ZOOM: (u32, u32) = (100, 200);
/// Accepted window size in logical points: small enough to stay usable, large
/// enough for a maximised 4K window.
/// Accepted window size range. The minimums are deliberately small: a window
/// the user resized short has to come back short. A 760px floor used to swallow
/// every shorter height, so the window reopened at a size nobody chose.
const WINDOW_SIZE: (u32, u32) = (400, 20_000);
/// Accepted window position. Generous because multi-monitor setups legitimately
/// place windows at negative coordinates.
const WINDOW_POS: i32 = 32_000;

/// Persisted preferences. Missing fields fall back to [`Default`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `"system" | "zh-CN" | "en-US"`.
    pub language: String,
    /// Empty means "next to the archive".
    pub default_extract_dir: String,
    /// `"ask" | "overwrite" | "skip" | "rename"`.
    pub overwrite_policy: String,
    /// Register per-user file associations (HKCU). **On by default**: a fresh
    /// install is expected to open archives with ZipNest, so the app claims the
    /// handler on its first run and the installer does the same. Unticking it
    /// removes the registration and is remembered.
    pub associate: bool,
    /// Register the Explorer context menu (HKCU). On by default, for the same
    /// reason: an unused right-click menu is dead weight nobody goes looking
    /// for.
    pub context_menu: bool,
    /// Which archive extensions ZipNest claims as the default handler.
    ///
    /// Empty means "all of them" (the fresh-profile and installer case); the
    /// Settings dialog's per-extension checkboxes write this list, so a user can
    /// keep `.tar` with their own tools and still have `.zip` open here.
    pub assoc_extensions: Vec<String>,
    /// Set once the Explorer integration has been applied or explicitly
    /// refused. While it is `false` the app applies the current settings' 
    /// integration at startup (the fresh-install path); afterwards it never
    /// does, so a deliberate opt-out is not undone on the next launch.
    pub integration_applied: bool,
    /// Close the window by itself once a create or extract job finishes. Off by
    /// default: the window carries the "done" message, and a window that
    /// disappears on its own takes that message with it.
    pub auto_close_after_job: bool,
    /// Cap for in-app previews, in bytes.
    pub preview_max_bytes: u64,
    /// UI zoom percent, 100 = 100%. Fixed value applied at startup; the app
    /// never re-zooms per frame (that caused fullscreen flicker).
    pub ui_zoom: u32,
    /// Auto-check for updates when the app starts.
    pub auto_check_update: bool,
    /// Total uncompressed bytes allowed for a single extract (zip-bomb guard).
    pub max_extract_bytes: u64,
    /// `"system" | "light" | "dark"`.
    pub theme_mode: String,
    /// Last window size in logical points; `None` on the first run.
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,

    /// Last window position; `None` lets the OS place the window.
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            default_extract_dir: String::new(),
            overwrite_policy: "ask".into(),
            associate: true,
            context_menu: true,
            // Explicitly all nine rather than an empty "means everything" list:
            // the Settings dialog has a "none" state too, and a missing field in
            // an older settings.json has to keep the old behaviour (everything
            // claimed) instead of releasing every association.
            assoc_extensions: crate::shell::SUPPORTED_EXTENSIONS
                .iter()
                .map(|e| (*e).to_string())
                .collect(),
            integration_applied: false,
            auto_close_after_job: false,
            preview_max_bytes: 8 * 1024 * 1024,
            ui_zoom: 100,
            auto_check_update: true,
            max_extract_bytes: DEFAULT_MAX_EXTRACT_BYTES,
            theme_mode: "system".into(),
            window_width: None,
            window_height: None,

            window_x: None,
            window_y: None,
        }
    }
}

/// Partial update: only the fields present are applied.
#[derive(Debug, Default, Deserialize)]
pub struct SettingsPatch {
    pub language: Option<String>,
    pub default_extract_dir: Option<String>,
    pub overwrite_policy: Option<String>,
    pub associate: Option<bool>,
    pub context_menu: Option<bool>,
    pub assoc_extensions: Option<Vec<String>>,
    pub integration_applied: Option<bool>,
    pub auto_close_after_job: Option<bool>,
    pub preview_max_bytes: Option<u64>,
    pub ui_zoom: Option<u32>,
    pub auto_check_update: Option<bool>,
    pub max_extract_bytes: Option<u64>,
    pub theme_mode: Option<String>,

    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
}

fn invalid() -> IpcError {
    IpcError::new("error.settings.invalid")
}

impl Settings {
    /// Apply a patch in place, validating enum-like fields against the known
    /// sets so an untrusted frontend can never persist a bad value.
    pub fn apply(&mut self, patch: SettingsPatch) -> Result<(), IpcError> {
        if let Some(v) = patch.language {
            if !LANGUAGES.contains(&v.as_str()) {
                return Err(invalid());
            }
            self.language = v;
        }
        if let Some(v) = patch.overwrite_policy {
            if !OVERWRITE.contains(&v.as_str()) {
                return Err(invalid());
            }
            self.overwrite_policy = v;
        }
        if let Some(v) = patch.default_extract_dir {
            self.default_extract_dir = v;
        }
        if let Some(v) = patch.associate {
            self.associate = v;
        }
        if let Some(v) = patch.context_menu {
            self.context_menu = v;
        }
        if let Some(v) = patch.assoc_extensions {
            // Canonicalised, never stored raw: a hand-edited file cannot make us
            // register a handler we do not ship.
            self.assoc_extensions = crate::shell::normalise_extensions(&v);
        }
        if let Some(v) = patch.integration_applied {
            self.integration_applied = v;
        }
        if let Some(v) = patch.auto_close_after_job {
            self.auto_close_after_job = v;
        }
        if let Some(v) = patch.preview_max_bytes {
            self.preview_max_bytes = v.clamp(PREVIEW_MAX_BYTES.0, PREVIEW_MAX_BYTES.1);
        }
        if let Some(v) = patch.ui_zoom {
            self.ui_zoom = v.clamp(UI_ZOOM.0, UI_ZOOM.1);
        }
        if let Some(v) = patch.auto_check_update {
            self.auto_check_update = v;
        }
        if let Some(v) = patch.max_extract_bytes {
            self.max_extract_bytes = v.clamp(EXTRACT_MAX_BYTES.0, EXTRACT_MAX_BYTES.1);
        }
        if let Some(v) = patch.theme_mode {
            if !THEMES.contains(&v.as_str()) {
                return Err(invalid());
            }
            self.theme_mode = v;
        }

        if let Some(v) = patch.window_width {
            self.window_width = Some(v.clamp(WINDOW_SIZE.0, WINDOW_SIZE.1));
        }
        if let Some(v) = patch.window_height {
            self.window_height = Some(v.clamp(WINDOW_SIZE.0, WINDOW_SIZE.1));
        }
        if let Some(v) = patch.window_x {
            self.window_x = Some(v.clamp(-WINDOW_POS, WINDOW_POS));
        }
        if let Some(v) = patch.window_y {
            self.window_y = Some(v.clamp(-WINDOW_POS, WINDOW_POS));
        }
        Ok(())
    }

    /// Repair values that were read from disk: bad enum strings fall back to
    /// their default and numeric fields are clamped into range. Never fails.
    fn sanitize(&mut self) {
        if !LANGUAGES.contains(&self.language.as_str()) {
            self.language = "system".into();
        }
        if !OVERWRITE.contains(&self.overwrite_policy.as_str()) {
            self.overwrite_policy = "ask".into();
        }
        if !THEMES.contains(&self.theme_mode.as_str()) {
            self.theme_mode = "system".into();
        }
        self.assoc_extensions = crate::shell::normalise_extensions(&self.assoc_extensions);
        self.preview_max_bytes =
            self.preview_max_bytes.clamp(PREVIEW_MAX_BYTES.0, PREVIEW_MAX_BYTES.1);
        self.ui_zoom = self.ui_zoom.clamp(UI_ZOOM.0, UI_ZOOM.1);
        self.max_extract_bytes =
            self.max_extract_bytes.clamp(EXTRACT_MAX_BYTES.0, EXTRACT_MAX_BYTES.1);
        self.window_width = self
            .window_width
            .map(|v| v.clamp(WINDOW_SIZE.0, WINDOW_SIZE.1));
        self.window_height = self
            .window_height
            .map(|v| v.clamp(WINDOW_SIZE.0, WINDOW_SIZE.1));
        self.window_x = self.window_x.map(|v| v.clamp(-WINDOW_POS, WINDOW_POS));
        self.window_y = self.window_y.map(|v| v.clamp(-WINDOW_POS, WINDOW_POS));
    }
}

/// What happened when a settings file could not be read as settings.
///
/// Kept out of [`Settings`] on purpose: it describes one load, not a
/// preference, so it must never be persisted (a flag inside `Settings` would
/// be one more field to serialise, sanitise and get wrong, and it would make a
/// recovered store unequal to its defaults in every test). The UI reads it from
/// the store instead — see [`SettingsStore::corrupt_settings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorruptSettings {
    /// Where the unreadable original was preserved, or `None` when it could not
    /// be (the directory is read-only, say). Worth showing to the user: it is
    /// the file their old preferences live in.
    pub backup: Option<PathBuf>,
    /// Parser message, for diagnostics — never shown verbatim (it is English
    /// and mentions byte positions the user cannot act on).
    pub detail: String,
}

/// Settings backed by a JSON file.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    current: Settings,
    /// Set only by a load that had to fall back because the file on disk was
    /// unusable; `None` for a normal load. Not persisted and not cleared by a
    /// later [`set`]: it records what the user lost at startup, and the UI's
    /// one-time notice is about exactly that.
    corrupted: Option<CorruptSettings>,
}

impl SettingsStore {
    /// Load (or default) from `path`; nothing is written until [`set`].
    pub fn new(path: PathBuf) -> Self {
        let (current, corrupted) = load_from(&path);
        Self {
            path,
            current,
            corrupted,
        }
    }

    /// `true` when the stored file could not be parsed and the defaults are in
    /// use because of it, i.e. the user's saved preferences were **not** read.
    ///
    /// This is the one thing the UI needs for a startup notice; pair it with
    /// [`corrupt_settings`] to name the backup file. False for a missing file
    /// (a fresh profile, nothing was lost), for a file that could not be
    /// *opened* (locked/ACL — it is left untouched and may load fine next
    /// launch) and for every successful load.
    pub fn was_reset_from_corrupt(&self) -> bool {
        self.corrupted.is_some()
    }

    /// The failure behind [`was_reset_from_corrupt`], with the backup path.
    pub fn corrupt_settings(&self) -> Option<&CorruptSettings> {
        self.corrupted.as_ref()
    }

    pub fn get(&self) -> &Settings {
        &self.current
    }

    /// Validate + persist a patch, returning the resulting settings.
    pub fn set(&mut self, patch: SettingsPatch) -> Result<Settings, IpcError> {
        let mut next = self.current.clone();
        next.apply(patch)?;
        save_to(&self.path, &next).map_err(|_| IpcError::new("error.io"))?;
        self.current = next;
        Ok(self.current.clone())
    }
}

/// Read `path`, returning the settings to use and — when the file had to be
/// quarantined as unreadable — what happened.
fn load_from(path: &Path) -> (Settings, Option<CorruptSettings>) {
    // Bytes first, not `read_to_string`: the two failures need opposite
    // treatment. Bytes that are not a settings document are *corruption* and
    // have to be preserved (below). A file that cannot even be opened —
    // another process holds it, an ACL says no — is not corruption, and
    // renaming it out from under its owner would turn a temporary condition
    // into permanent data loss.
    let raw = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // No file at all: a fresh profile, nothing was lost.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Settings::default(), None);
        }
        // Unreadable: use defaults for now and leave the file where it is.
        Err(_) => return (Settings::default(), None),
    };

    match parse_settings(&raw) {
        Ok(mut s) => {
            s.sanitize();
            (s, None)
        }
        Err(detail) => {
            // The document is unusable, so keeping it at `path` only means the
            // next `set()` writes defaults over it. Move it aside first: the
            // bytes are all that is left of the user's preferences.
            let backup = quarantine_corrupt(path);
            (
                Settings::default(),
                Some(CorruptSettings { backup, detail }),
            )
        }
    }
}

fn parse_settings(raw: &[u8]) -> Result<Settings, String> {
    let text = std::str::from_utf8(raw).map_err(|e| format!("not UTF-8: {e}"))?;
    // Notepad and plenty of Windows editors save UTF-8 *with* a BOM. Handed
    // straight to serde_json, that first character makes the whole document
    // invalid — the store then fell back to defaults and the next save wrote
    // those over every preference the user had. Strip it (only at the start:
    // a stray BOM inside the document is still a broken document).
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
}

/// Preserve an unparsable `settings.json` as `settings.json.corrupt-<unix
/// seconds>` next to it (same directory, so the rename stays on one
/// filesystem and the user can find it).
///
/// Returns the path the bytes ended up at, or `None` when they could not be
/// kept. A copy is attempted when the rename is refused: on Windows a file
/// another process (an AV scanner, an editor) has open cannot be renamed, but
/// it usually can be read.
fn quarantine_corrupt(path: &Path) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_string());
    // Never replace an earlier backup: a crash loop can corrupt twice inside
    // the same second, and the second failure must not delete the first file.
    for attempt in 0..8u32 {
        let name = if attempt == 0 {
            format!("{base}.corrupt-{stamp}")
        } else {
            format!("{base}.corrupt-{stamp}-{attempt}")
        };
        let candidate = path.with_file_name(name);
        if candidate.exists() {
            continue;
        }
        if std::fs::rename(path, &candidate).is_ok() {
            return Some(candidate);
        }
        // The original is still in place; keep a copy of the bytes so a later
        // save that overwrites `path` cannot destroy them.
        return std::fs::copy(path, &candidate).ok().map(|_| candidate);
    }
    None
}

/// `settings.json` -> `settings.json.tmp`, in the same directory so the final
/// rename stays on one filesystem.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Persist `s` atomically: write a sibling temp file, flush it to disk, then
/// rename over the target. A failed rename leaves the previous file intact.
fn save_to(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    // Never fall back to an empty string: writing it would wipe the user's file.
    let json = serde_json::to_string_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = tmp_path_for(path);
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = written {
        // Nothing was replaced yet; drop the partial temp copy.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Keep the old file; only the temp copy is dropped.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// `%APPDATA%\ZipNest\settings.json` on Windows, `$XDG_CONFIG_HOME`/`~/.config`
/// elsewhere, temp dir as a last resort.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("ZipNest").join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    /// A window the user resized small has to come back the same size: a 760px
    /// floor used to clamp every shorter height back up, so the window reopened
    /// at a size the user never chose.
    #[test]
    fn a_small_window_is_remembered() {
        let mut s = Settings::default();
        s.apply(SettingsPatch {
            window_width: Some(600),
            window_height: Some(400),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.window_width, Some(600));
        assert_eq!(s.window_height, Some(400));
        // Sizes no layout can live in are still rejected.
        s.apply(SettingsPatch {
            window_width: Some(50),
            window_height: Some(10),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.window_width, Some(WINDOW_SIZE.0));
        assert_eq!(s.window_height, Some(WINDOW_SIZE.0));
    }

    fn tmp_path() -> PathBuf {
        crate::sweep_stale_test_temp("zipnest-settings-");
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("zipnest-settings-{}-{}", std::process::id(), n))
            .join("settings.json")
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.language, "system");
        assert_eq!(s.overwrite_policy, "ask");
        // Integration is claimed out of the box (v0.4.8): a fresh install opens
        // archives with ZipNest without the user hunting for a checkbox.
        assert!(s.associate && s.context_menu);
        // ... but the one-time bootstrap has not run yet.
        assert!(!s.integration_applied);
        assert!(s.preview_max_bytes > 0);
    }

    #[test]
    fn an_explicit_opt_out_survives_a_restart() {
        // The bootstrap must run exactly once: unchecking the box has to stick,
        // otherwise every launch would re-claim the associations.
        let path = tmp_path();
        {
            let mut store = SettingsStore::new(path.clone());
            store
                .set(SettingsPatch {
                    associate: Some(false),
                    context_menu: Some(false),
                    integration_applied: Some(true),
                    ..Default::default()
                })
                .unwrap();
        }
        let reopened = SettingsStore::new(path);
        let s = reopened.get();
        assert!(!s.associate && !s.context_menu);
        assert!(s.integration_applied, "opted out, and the bootstrap must not run again");
    }

    #[test]
    fn an_older_settings_file_without_the_flag_opts_in() {
        // v0.4.7 and earlier never wrote `integration_applied`, so the field
        // reads back as false and the app applies whatever the file says —
        // removals for a user who turned it off, registration for everyone
        // else.
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"language":"zh-CN","associate":false}"#).unwrap();
        let store = SettingsStore::new(path);
        assert!(!store.get().integration_applied);
        assert!(!store.get().associate, "the recorded opt-out is honoured");
    }

    #[test]
    fn missing_file_yields_defaults() {
        let store = SettingsStore::new(tmp_path());
        assert_eq!(store.get(), &Settings::default());
    }

    #[test]
    fn round_trips_through_disk() {
        let path = tmp_path();
        {
            let mut store = SettingsStore::new(path.clone());
            store
                .set(SettingsPatch {
                    language: Some("en-US".into()),
                    associate: Some(true),
                    ..Default::default()
                })
                .unwrap();
        }
        let reopened = SettingsStore::new(path);
        assert_eq!(reopened.get().language, "en-US");
        assert!(reopened.get().associate);
        // Untouched fields keep their defaults.
        assert_eq!(reopened.get().overwrite_policy, "ask");
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(SettingsStore::new(path).get(), &Settings::default());
    }

    /// Every `*.corrupt-*` file the store left in `path`'s directory.
    fn corrupt_backups(path: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().contains(".corrupt-"))
                    .unwrap_or(false)
            })
            .collect();
        found.sort();
        found
    }

    /// M2: a corrupt file used to fall back to defaults *silently*, so the next
    /// `set()` wrote those defaults over every preference the user had. The
    /// bytes now survive as a sibling backup, and the caller is told.
    #[test]
    fn a_corrupt_file_is_backed_up_before_defaults_are_used() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        let store = SettingsStore::new(path.clone());

        // Defaults are in use, and the store says why.
        assert_eq!(store.get(), &Settings::default());
        assert!(store.was_reset_from_corrupt());
        let corrupt = store
            .corrupt_settings()
            .expect("a corrupt load must be reported to the caller");
        let backup = corrupt.backup.as_ref().expect("the original must be kept");
        // `settings.json.corrupt-<unix seconds>`, in the same directory.
        let name = backup.file_name().unwrap().to_string_lossy().into_owned();
        let stamp = name
            .strip_prefix("settings.json.corrupt-")
            .unwrap_or_else(|| panic!("unexpected backup name: {name}"));
        assert!(
            stamp.parse::<u64>().is_ok(),
            "backup suffix must be a unix timestamp, got {stamp}"
        );
        assert_eq!(backup.parent(), path.parent(), "backup stays next to the original");
        // The original is gone from its old name; its bytes survive intact.
        assert!(!path.exists(), "the unparsable file must be moved aside");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "{ not json");
        assert_eq!(corrupt_backups(&path).len(), 1);
    }

    /// The whole point of the backup: the defaults the store now holds are
    /// never written over the only copy of the user's preferences.
    #[test]
    fn a_save_after_corruption_does_not_destroy_the_backup() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"language":"zh-CN","ui_zoom":150"#).unwrap();
        let mut store = SettingsStore::new(path.clone());
        let backup = store
            .corrupt_settings()
            .unwrap()
            .backup
            .clone()
            .unwrap();
        store
            .set(SettingsPatch {
                language: Some("en-US".into()),
                ..Default::default()
            })
            .unwrap();
        // The new file is a clean default-plus-patch document...
        assert_eq!(
            serde_json::from_str::<Settings>(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .language,
            "en-US"
        );
        // ...and the broken one still sits beside it, byte for byte.
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            r#"{"language":"zh-CN","ui_zoom":150"#
        );
    }

    #[test]
    fn a_good_file_is_never_quarantined() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"language":"en-US"}"#).unwrap();
        let store = SettingsStore::new(path.clone());
        assert_eq!(store.get().language, "en-US");
        assert!(!store.was_reset_from_corrupt());
        assert!(store.corrupt_settings().is_none());
        assert!(corrupt_backups(&path).is_empty());
        assert!(path.exists(), "the file must be left exactly where it was");
    }

    /// A missing file is a fresh profile, not a loss: no backup, no warning.
    #[test]
    fn a_missing_file_is_not_corruption() {
        let store = SettingsStore::new(tmp_path());
        assert!(!store.was_reset_from_corrupt());
        assert!(store.corrupt_settings().is_none());
    }

    /// Two failures inside one second (a restart loop, a test) must not make
    /// the second backup overwrite the first.
    #[test]
    fn a_second_corruption_keeps_the_first_backup() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ first").unwrap();
        let first = SettingsStore::new(path.clone())
            .corrupt_settings()
            .unwrap()
            .backup
            .clone()
            .unwrap();
        std::fs::write(&path, "{ second").unwrap();
        let second = SettingsStore::new(path.clone())
            .corrupt_settings()
            .unwrap()
            .backup
            .clone()
            .unwrap();
        assert_ne!(first, second, "the second backup must not reuse the name");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "{ first");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "{ second");
    }

    /// Bytes that are not UTF-8 are corruption too, and are preserved raw —
    /// reading them back through a lossy string would destroy the evidence.
    #[test]
    fn a_non_utf8_file_is_quarantined() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let raw: [u8; 4] = [b'{', 0xff, 0xfe, b'}'];
        std::fs::write(&path, raw).unwrap();
        let store = SettingsStore::new(path.clone());
        assert_eq!(store.get(), &Settings::default());
        let backup = store.corrupt_settings().unwrap().backup.clone().unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), raw);
        assert!(!path.exists());
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_fields() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"language":"zh-CN"}"#).unwrap();
        let store = SettingsStore::new(path);
        assert_eq!(store.get().language, "zh-CN");
        assert_eq!(store.get().preview_max_bytes, Settings::default().preview_max_bytes);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"language":"en-US","future":true}"#).unwrap();
        assert_eq!(SettingsStore::new(path).get().language, "en-US");
    }

    #[test]
    fn a_bom_prefixed_file_is_still_read() {
        // Notepad writes UTF-8 with a BOM by default. Treating that BOM as
        // corruption threw away every preference and then overwrote the file
        // with defaults on the next save.
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "\u{feff}{\"language\":\"zh-CN\",\"overwrite_policy\":\"rename\"}")
            .unwrap();
        let store = SettingsStore::new(path);
        assert_eq!(store.get().language, "zh-CN");
        assert_eq!(store.get().overwrite_policy, "rename");
    }

    #[test]
    fn only_a_leading_bom_is_stripped() {
        // A BOM anywhere else is still a broken document, not a silent reset
        // trigger we should paper over.
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"language\":\"zh-CN\"\u{feff}}").unwrap();
        assert_eq!(SettingsStore::new(path).get().language, "system");
    }

    #[test]
    fn invalid_values_are_rejected_and_not_persisted() {
        let path = tmp_path();
        let mut store = SettingsStore::new(path);
        let err = store
            .set(SettingsPatch {
                language: Some("klingon".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.key, "error.settings.invalid");
        assert_eq!(store.get().language, "system");
    }

    #[test]
    fn save_writes_a_complete_file_and_leaves_no_temp() {
        let path = tmp_path();
        let mut store = SettingsStore::new(path.clone());
        store
            .set(SettingsPatch {
                language: Some("en-US".into()),
                ui_zoom: Some(150),
                ..Default::default()
            })
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: Settings = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.language, "en-US");
        assert_eq!(parsed.ui_zoom, 150);
        assert!(!tmp_path_for(&path).exists(), "temp file must not linger");
    }

    #[test]
    fn failed_rename_keeps_target_and_drops_temp() {
        let path = tmp_path();
        // A directory at the target path makes the final rename fail.
        std::fs::create_dir_all(&path).unwrap();
        assert!(save_to(&path, &Settings::default()).is_err());
        assert!(path.is_dir(), "target must survive a failed rename");
        assert!(!tmp_path_for(&path).exists(), "temp file must be cleaned up");
    }

    #[test]
    fn failed_save_keeps_previous_file_content() {
        let path = tmp_path();
        let mut store = SettingsStore::new(path.clone());
        store
            .set(SettingsPatch {
                language: Some("zh-CN".into()),
                ..Default::default()
            })
            .unwrap();
        // A directory where the temp file goes makes the next save fail early.
        std::fs::create_dir_all(tmp_path_for(&path)).unwrap();
        let err = store
            .set(SettingsPatch {
                language: Some("en-US".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.key, "error.io");
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: Settings = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.language, "zh-CN", "old file must survive a failed save");
        assert_eq!(store.get().language, "zh-CN");
    }

    #[test]
    fn dirty_config_is_sanitized_on_load() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"language":"klingon","overwrite_policy":"junk","preview_max_bytes":18446744073709551615,"ui_zoom":9999}"#,
        )
        .unwrap();
        let s = SettingsStore::new(path).get().clone();
        assert_eq!(s.language, "system");
        assert_eq!(s.overwrite_policy, "ask");
        assert_eq!(s.preview_max_bytes, PREVIEW_MAX_BYTES.1);
        assert_eq!(s.ui_zoom, UI_ZOOM.1);
        // Unknown-to-the-file field still takes its default.
        assert_eq!(s.max_extract_bytes, DEFAULT_MAX_EXTRACT_BYTES);
    }

    #[test]
    fn zeros_and_high_zoom_are_clamped_on_load() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"preview_max_bytes":0,"ui_zoom":1,"max_extract_bytes":1}"#,
        )
        .unwrap();
        let s = SettingsStore::new(path).get().clone();
        assert_eq!(s.preview_max_bytes, PREVIEW_MAX_BYTES.0);
        assert_eq!(s.ui_zoom, UI_ZOOM.0);
        assert_eq!(s.max_extract_bytes, EXTRACT_MAX_BYTES.0);
    }

    #[test]
    fn max_extract_bytes_default_and_clamp() {
        assert_eq!(Settings::default().max_extract_bytes, 64 * 1024 * 1024 * 1024);
        let mut s = Settings::default();
        s.apply(SettingsPatch {
            max_extract_bytes: Some(0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.max_extract_bytes, EXTRACT_MAX_BYTES.0);
        s.apply(SettingsPatch {
            max_extract_bytes: Some(u64::MAX),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.max_extract_bytes, EXTRACT_MAX_BYTES.1);
        s.apply(SettingsPatch {
            max_extract_bytes: Some(2 * 1024 * 1024 * 1024),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.max_extract_bytes, 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn max_extract_bytes_is_clamped_on_load() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"max_extract_bytes":18446744073709551615}"#).unwrap();
        assert_eq!(
            SettingsStore::new(path).get().max_extract_bytes,
            EXTRACT_MAX_BYTES.1
        );
    }

    #[test]
    fn legacy_file_without_max_extract_bytes_gets_default() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"language":"zh-CN","ui_zoom":120}"#).unwrap();
        let store = SettingsStore::new(path);
        assert_eq!(store.get().language, "zh-CN");
        assert_eq!(store.get().ui_zoom, 120);
        assert_eq!(store.get().max_extract_bytes, Settings::default().max_extract_bytes);
    }

    #[test]
    fn theme_and_window_geometry_are_validated() {
        let mut store = SettingsStore::new(tmp_path());
        assert_eq!(store.get().theme_mode, "system");
        assert!(store.get().window_width.is_none());
        // An unknown theme is rejected outright, like an unknown language.
        assert!(store
            .set(SettingsPatch {
                theme_mode: Some("neon".into()),
                ..Default::default()
            })
            .is_err());
        store
            .set(SettingsPatch {
                theme_mode: Some("dark".into()),
                window_width: Some(50),       // below the minimum
                window_height: Some(1200),
                window_x: Some(-999_999),     // out of range
                window_y: Some(40),
                ..Default::default()
            })
            .unwrap();
        let s = store.get();
        assert_eq!(s.theme_mode, "dark");
        assert_eq!(s.window_width, Some(WINDOW_SIZE.0));
        assert_eq!(s.window_height, Some(1200));
        assert_eq!(s.window_x, Some(-WINDOW_POS));
        assert_eq!(s.window_y, Some(40));
    }

    #[test]
    fn a_dirty_theme_in_the_file_is_repaired() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"theme_mode":"klingon","window_width":5}"#).unwrap();
        let s = SettingsStore::new(path);
        assert_eq!(s.get().theme_mode, "system");
        assert_eq!(s.get().window_width, Some(WINDOW_SIZE.0));
    }
}

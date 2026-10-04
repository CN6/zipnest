//! User settings: a small JSON document persisted under the app data dir.
//!
//! The store takes its file path as a parameter so tests can point it at a
//! temp dir. Load never fails: a missing or corrupt file falls back to
//! [`Settings::default`], and unknown fields are ignored so an older build can
//! still read a newer file. Values read back from disk are sanitized (bad enum
//! strings fall back to defaults, numbers are clamped) and saves are atomic
//! (temp file + rename), so a hand-edited or half-written file is harmless.

use crate::error::IpcError;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const LANGUAGES: [&str; 3] = ["system", "zh-CN", "en-US"];
const OVERWRITE: [&str; 4] = ["ask", "overwrite", "skip", "rename"];

/// Accepted range for [`Settings::preview_max_bytes`]: 1 KiB ..= 512 MiB.
const PREVIEW_MAX_BYTES: (u64, u64) = (1024, 512 * 1024 * 1024);
/// Accepted range for [`Settings::max_extract_bytes`]: 1 MiB ..= 16 TiB.
const EXTRACT_MAX_BYTES: (u64, u64) = (1024 * 1024, 16 * 1024 * 1024 * 1024 * 1024);
/// Default extract quota: large enough for real archives, small enough to stop
/// a decompression bomb.
const DEFAULT_MAX_EXTRACT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Accepted range for [`Settings::ui_zoom`].
const UI_ZOOM: (u32, u32) = (100, 200);

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
    /// Register per-user file associations (HKCU).
    pub associate: bool,
    /// Register the Explorer context menu (HKCU).
    pub context_menu: bool,
    /// Cap for in-app previews, in bytes.
    pub preview_max_bytes: u64,
    /// UI zoom percent, 100 = 100%. Fixed value applied at startup; the app
    /// never re-zooms per frame (that caused fullscreen flicker).
    pub ui_zoom: u32,
    /// Auto-check for updates when the app starts.
    pub auto_check_update: bool,
    /// Total uncompressed bytes allowed for a single extract (zip-bomb guard).
    pub max_extract_bytes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            default_extract_dir: String::new(),
            overwrite_policy: "ask".into(),
            associate: false,
            context_menu: false,
            preview_max_bytes: 8 * 1024 * 1024,
            ui_zoom: 100,
            auto_check_update: true,
            max_extract_bytes: DEFAULT_MAX_EXTRACT_BYTES,
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
    pub preview_max_bytes: Option<u64>,
    pub ui_zoom: Option<u32>,
    pub auto_check_update: Option<bool>,
    pub max_extract_bytes: Option<u64>,
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
        self.preview_max_bytes =
            self.preview_max_bytes.clamp(PREVIEW_MAX_BYTES.0, PREVIEW_MAX_BYTES.1);
        self.ui_zoom = self.ui_zoom.clamp(UI_ZOOM.0, UI_ZOOM.1);
        self.max_extract_bytes =
            self.max_extract_bytes.clamp(EXTRACT_MAX_BYTES.0, EXTRACT_MAX_BYTES.1);
    }
}

/// Settings backed by a JSON file.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    current: Settings,
}

impl SettingsStore {
    /// Load (or default) from `path`; nothing is written until [`set`].
    pub fn new(path: PathBuf) -> Self {
        let current = load_from(&path);
        Self { path, current }
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

fn load_from(path: &Path) -> Settings {
    let mut s: Settings = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    s.sanitize();
    s
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

    fn tmp_path() -> PathBuf {
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
        assert!(!s.associate && !s.context_menu);
        assert!(s.preview_max_bytes > 0);
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
}

//! User settings: a small JSON document persisted under the app data dir.
//!
//! The store takes its file path as a parameter so tests can point it at a
//! temp dir. Load never fails: a missing or corrupt file falls back to
//! [`Settings::default`], and unknown fields are ignored so an older build can
//! still read a newer file.

use crate::error::IpcError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const LANGUAGES: [&str; 3] = ["system", "zh-CN", "en-US"];
const OVERWRITE: [&str; 4] = ["ask", "overwrite", "skip", "rename"];

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
            self.preview_max_bytes = v;
        }
        Ok(())
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
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(s).unwrap_or_default();
    std::fs::write(path, json)
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
}

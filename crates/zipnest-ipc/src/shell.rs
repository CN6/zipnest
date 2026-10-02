//! Per-user Explorer integration (HKCU only).
//!
//! Everything here is **HKCU** on purpose: machine-wide `HKLM` registration
//! needs elevation, which the product never requests (spec §10). The registry
//! layout is produced by pure functions so it can be asserted in tests; the
//! actual writes go through [`ShellApplier`], which the app wires to `reg.exe`
//! and tests wire to a recorder.

use serde::{Deserialize, Serialize};

/// Archive extensions ZipNest can open, in the order the UI lists them.
pub const SUPPORTED_EXTENSIONS: [&str; 9] =
    ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"];

/// Which parts of the integration to (un)register.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ShellOptions {
    pub associate: bool,
    pub context_menu: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegKind {
    Sz,
    ExpandSz,
    Dword,
}

/// A single registry mutation. Add and remove share one shape so an applier
/// cannot drift from the generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegOp {
    SetValue {
        key: String,
        /// `None` targets the key's default value.
        name: Option<String>,
        value: String,
        kind: RegKind,
    },
    DeleteKey {
        key: String,
    },
    DeleteValue {
        key: String,
        name: String,
    },
}

impl RegOp {
    fn set(key: impl Into<String>, name: Option<&str>, value: impl Into<String>) -> Self {
        RegOp::SetValue {
            key: key.into(),
            name: name.map(str::to_string),
            value: value.into(),
            kind: RegKind::Sz,
        }
    }
}

const CLASSES: &str = r"HKCU\Software\Classes";

fn prog_id(ext: &str) -> String {
    format!("ZipNest.{ext}")
}

/// `"<exe>"` — quoted so a path with spaces still resolves.
fn quoted(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

/// `open` command line: `"<exe>" "%1"`.
fn open_command(exe: &std::path::Path) -> String {
    format!("{} \"%1\"", quoted(exe))
}

/// File-association ops: a ProgID per extension that becomes the **default**
/// handler (writing the extension's default value), plus an `OpenWithProgids`
/// entry so the user can still switch back without losing us.
pub fn assoc_ops(exe: &std::path::Path) -> Vec<RegOp> {
    let icon = format!("{},0", quoted(exe));
    let mut ops = Vec::new();
    for ext in SUPPORTED_EXTENSIONS {
        let pid = prog_id(ext);
        ops.push(RegOp::set(
            format!(r"{CLASSES}\{pid}"),
            None,
            format!("ZipNest {ext} archive"),
        ));
        ops.push(RegOp::set(format!(r"{CLASSES}\{pid}\DefaultIcon"), None, icon.clone()));
        ops.push(RegOp::set(
            format!(r"{CLASSES}\{pid}\shell\open\command"),
            None,
            open_command(exe),
        ));
        // Make ZipNest the default opener for this extension.
        ops.push(RegOp::set(
            format!(r"{CLASSES}\.{ext}"),
            None,
            pid.clone(),
        ));
        // Keep an OpenWithProgids entry as a graceful fallback.
        ops.push(RegOp::set(
            format!(r"{CLASSES}\.{ext}\OpenWithProgids"),
            Some(&pid),
            "",
        ));
    }
    ops
}

/// Undo [`assoc_ops`]: drop each ProgID tree and its OpenWithProgids entry.
/// The extension's default value is left untouched (restoring the previous
/// opener is the user's call — Windows keeps the old handler in UserChoice).
pub fn assoc_removals() -> Vec<RegOp> {
    let mut ops = Vec::new();
    for ext in SUPPORTED_EXTENSIONS {
        let pid = prog_id(ext);
        ops.push(RegOp::DeleteKey { key: format!(r"{CLASSES}\{pid}") });
        ops.push(RegOp::DeleteValue {
            key: format!(r"{CLASSES}\.{ext}\OpenWithProgids"),
            name: pid,
        });
    }
    ops
}

/// One Explorer context target. Kept separate because a hardened machine can
/// deny `*\shell` (all files) or `Directory\Background\shell` while still
/// allowing the others, so the UI reports them independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuTarget {
    File,
    Directory,
    Background,
}

fn menu_key(target: MenuTarget) -> String {
    let base = match target {
        MenuTarget::File => r"*\shell\ZipNest",
        MenuTarget::Directory => r"Directory\shell\ZipNest",
        MenuTarget::Background => r"Directory\Background\shell\ZipNest",
    };
    format!(r"{CLASSES}\{base}")
}

/// Context-menu ops for one target. Files and folders get `%1`; the desktop /
/// folder background gets `%V` (the folder itself).
pub fn menu_ops(target: MenuTarget, exe: &std::path::Path) -> Vec<RegOp> {
    let (base, arg) = match target {
        MenuTarget::File => (r"*\shell\ZipNest", "\"%1\""),
        MenuTarget::Directory => (r"Directory\shell\ZipNest", "\"%1\""),
        MenuTarget::Background => (r"Directory\Background\shell\ZipNest", "\"%V\""),
    };
    let key = format!(r"{CLASSES}\{base}");
    vec![
        RegOp::set(key.clone(), Some("MUIVerb"), "ZipNest"),
        RegOp::set(key.clone(), Some("Icon"), format!("{},0", quoted(exe))),
        RegOp::set(format!(r"{key}\command"), None, format!("{} {arg}", quoted(exe))),
    ]
}

pub fn file_menu_ops(exe: &std::path::Path) -> Vec<RegOp> {
    menu_ops(MenuTarget::File, exe)
}

pub fn directory_menu_ops(exe: &std::path::Path) -> Vec<RegOp> {
    menu_ops(MenuTarget::Directory, exe)
}

pub fn background_menu_ops(exe: &std::path::Path) -> Vec<RegOp> {
    menu_ops(MenuTarget::Background, exe)
}

/// Undo every context-menu entry.
pub fn context_menu_removals() -> Vec<RegOp> {
    [MenuTarget::File, MenuTarget::Directory, MenuTarget::Background]
        .iter()
        .map(|t| RegOp::DeleteKey { key: menu_key(*t) })
        .collect()
}

/// Outcome of a (possibly partial) integration update. `warnings` holds one
/// i18n key per component the OS denied, so a hardened machine blocks only
/// that piece instead of the whole save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellRegisterResult {
    pub associate: bool,
    pub context_menu: bool,
    pub warnings: Vec<String>,
}

/// Applies generated ops. Implemented for real by [`WindowsRegistry`] and by
/// a recorder in tests.
pub trait ShellApplier {
    fn run(&self, ops: &[RegOp]) -> std::io::Result<()>;
}

/// Writes directly through the Win32 registry API — no `reg.exe` child
/// process, no console flash, no UI blocking on a shell wait. This is the
/// only implementation users run; tests use their own recorder applier.
pub struct WindowsRegistry;

#[cfg(windows)]
mod win32 {
    use super::{RegKind, RegOp};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const HKEY_CURRENT_USER: isize = 0x8000_0001;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_MORE_DATA: i32 = 234;
    const KEY_SET_VALUE: u32 = 0x0002;
    const KEY_CREATE_SUB_KEY: u32 = 0x0004;
    const KEY_WOW64_64KEY: u32 = 0x0100;
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const REG_DWORD: u32 = 4;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegCreateKeyExW(
            hkey: isize,
            subkey: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            sam: u32,
            security: *const c_void,
            result: *mut isize,
            disposition: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            hkey: isize,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            len: u32,
        ) -> i32;
        fn RegDeleteTreeW(hkey: isize, subkey: *const u16) -> i32;
        fn RegDeleteValueW(hkey: isize, name: *const u16) -> i32;
        fn RegCloseKey(hkey: isize) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().collect()
    }

    /// `HKCU\Software\Classes\...` → (root, subkey, value name)
    fn split(key: &str) -> (isize, String) {
        let rest = key
            .strip_prefix("HKCU\\")
            .or_else(|| key.strip_prefix("hkcu\\"))
            .unwrap_or(key)
            .replace('/', "\\");
        (HKEY_CURRENT_USER, rest)
    }

    fn set_value(key: &str, name: Option<&str>, value: &str, kind: RegKind) -> i32 {
        let (root, sub) = split(key);
        let mut sub_w = wide(&sub); sub_w.push(0);
        let mut hkey = 0isize;
        let rc = unsafe {
            RegCreateKeyExW(
                root,
                sub_w.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE | KEY_CREATE_SUB_KEY | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS {
            return rc;
        }
        let name_w: Vec<u16> = match name { Some(n) => { let mut v = wide(n); v.push(0); v } None => vec![0] };
        let (reg_kind, data_bytes) = match kind {
            RegKind::Dword => {
                let n = value.parse::<u32>().unwrap_or(0);
                (REG_DWORD, n.to_ne_bytes().to_vec())
            }
            RegKind::ExpandSz => {
                let mut v = wide(value);
                v.push(0);
                let mut bytes = Vec::new();
                for u in &v {
                    bytes.extend_from_slice(&u.to_le_bytes());
                }
                (REG_EXPAND_SZ, bytes)
            }
            RegKind::Sz => {
                let mut v = wide(value);
                v.push(0);
                let mut bytes = Vec::new();
                for u in &v {
                    bytes.extend_from_slice(&u.to_le_bytes());
                }
                (REG_SZ, bytes)
            }
        };
        let rc = unsafe {
            RegSetValueExW(
                hkey,
                name_w.as_ptr(),
                0,
                reg_kind,
                data_bytes.as_ptr(),
                data_bytes.len() as u32,
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        rc
    }

    fn delete_key(key: &str) -> i32 {
        let (root, sub) = split(key);
        let mut sub_w = wide(&sub); sub_w.push(0);
        unsafe { RegDeleteTreeW(root, sub_w.as_ptr()) }
    }

    fn delete_value(key: &str, name: &str) -> i32 {
        let (root, sub) = split(key);
        let mut sub_w = wide(&sub); sub_w.push(0);
        let mut hkey = 0isize;
        let rc = unsafe {
            RegCreateKeyExW(
                root,
                sub_w.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS {
            return rc;
        }
        let mut name_w = wide(name); name_w.push(0);
        let rc = unsafe { RegDeleteValueW(hkey, name_w.as_ptr()) };
        unsafe {
            RegCloseKey(hkey);
        }
        rc
    }

    fn rc_to_io(rc: i32) -> std::io::Error {
        std::io::Error::from_raw_os_error(rc)
    }

    pub fn apply(ops: &[RegOp]) -> std::io::Result<()> {
        for op in ops {
            let rc = match op {
                RegOp::SetValue { key, name, value, kind } => set_value(key, name.as_deref(), value, *kind),
                RegOp::DeleteKey { key } => delete_key(key),
                RegOp::DeleteValue { key, name } => delete_value(key, name),
            };
            // Deleting something that was never registered is a no-op.
            if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND && rc != ERROR_MORE_DATA {
                return Err(rc_to_io(rc));
            }
        }
        Ok(())
    }
}

impl ShellApplier for WindowsRegistry {
    fn run(&self, ops: &[RegOp]) -> std::io::Result<()> {
        #[cfg(not(windows))]
        {
            let _ = ops;
            Ok(())
        }
        #[cfg(windows)]
        {
            win32::apply(ops)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn exe() -> &'static Path {
        // A path with spaces must stay quoted in every command line.
        Path::new(r"C:\Program Files\ZipNest\ZipNest.exe")
    }

    fn find_set<'a>(ops: &'a [RegOp], key: &str, name: Option<&str>) -> Option<&'a str> {
        ops.iter().find_map(|op| match op {
            RegOp::SetValue { key: k, name: n, value, .. }
                if k == key && n.as_deref() == name =>
            {
                Some(value.as_str())
            }
            _ => None,
        })
    }

    #[test]
    fn assoc_sets_open_command_with_quoted_exe() {
        let ops = assoc_ops(exe());
        let cmd = find_set(&ops, r"HKCU\Software\Classes\ZipNest.zip\shell\open\command", None);
        assert_eq!(cmd, Some(r#""C:\Program Files\ZipNest\ZipNest.exe" "%1""#));
    }

    #[test]
    fn assoc_makes_zipnest_the_default_handler() {
        let ops = assoc_ops(exe());
        for ext in SUPPORTED_EXTENSIONS {
            let pid = prog_id(ext);
            // The extension's default value points at our ProgID, and the
            // ProgID's open command is quoted.
            assert_eq!(
                find_set(&ops, &format!(r"HKCU\Software\Classes\.{ext}"), None),
                Some(pid.as_str()),
                "default handler missing for .{ext}"
            );
            let cmd_key = format!(r"HKCU\Software\Classes\{pid}\shell\open\command");
            assert!(find_set(&ops, &cmd_key, None).is_some(), "open cmd missing for {ext}");
        }
    }

    #[test]
    fn assoc_covers_every_supported_extension() {
        let ops = assoc_ops(exe());
        for ext in SUPPORTED_EXTENSIONS {
            let key = format!(r"HKCU\Software\Classes\ZipNest.{ext}\shell\open\command");
            assert!(find_set(&ops, &key, None).is_some(), "missing {ext}");
        }
    }

    #[test]
    fn assoc_removals_drop_the_same_progids() {
        let adds = assoc_ops(exe());
        let removals = assoc_removals();
        for ext in SUPPORTED_EXTENSIONS {
            let pid = prog_id(ext);
            assert!(adds.iter().any(|o| matches!(o, RegOp::SetValue { key, .. } if key == &format!(r"HKCU\Software\Classes\{pid}"))));
            assert!(removals.iter().any(|o| matches!(o, RegOp::DeleteKey { key } if key == &format!(r"HKCU\Software\Classes\{pid}"))));
        }
    }

    #[test]
    fn context_menu_uses_percent_one_and_percent_v() {
        let file_ops = file_menu_ops(exe());
        let bg_ops = background_menu_ops(exe());
        let file = find_set(&file_ops, r"HKCU\Software\Classes\*\shell\ZipNest\command", None);
        let bg = find_set(
            &bg_ops,
            r"HKCU\Software\Classes\Directory\Background\shell\ZipNest\command",
            None,
        );
        assert_eq!(file, Some(r#""C:\Program Files\ZipNest\ZipNest.exe" "%1""#));
        assert_eq!(bg, Some(r#""C:\Program Files\ZipNest\ZipNest.exe" "%V""#));
        assert_eq!(
            find_set(&file_ops, r"HKCU\Software\Classes\*\shell\ZipNest", Some("MUIVerb")),
            Some("ZipNest")
        );
    }

    #[test]
    fn directory_menu_uses_percent_one() {
        let ops = directory_menu_ops(exe());
        assert_eq!(
            find_set(&ops, r"HKCU\Software\Classes\Directory\shell\ZipNest\command", None),
            Some(r#""C:\Program Files\ZipNest\ZipNest.exe" "%1""#)
        );
    }

    #[test]
    fn context_menu_removals_cover_all_three_contexts() {
        let removals = context_menu_removals();
        assert_eq!(removals.len(), 3);
        assert!(removals.iter().all(|o| matches!(o, RegOp::DeleteKey { .. })));
    }
}


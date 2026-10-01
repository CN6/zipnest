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

/// File-association ops: a ProgID per extension plus an `OpenWithProgids`
/// entry. We *add an option* rather than hijack the default handler.
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
        // Empty REG_SZ under OpenWithProgids = "offer ZipNest, don't take over".
        ops.push(RegOp::set(
            format!(r"{CLASSES}\.{ext}\OpenWithProgids"),
            Some(&pid),
            "",
        ));
    }
    ops
}

/// Undo [`assoc_ops`]: drop each ProgID tree and its OpenWithProgids entry.
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

/// Writes through `reg.exe` (no extra crate, no elevation).
pub struct WindowsRegistry;

impl ShellApplier for WindowsRegistry {
    fn run(&self, ops: &[RegOp]) -> std::io::Result<()> {
        #[cfg(not(windows))]
        {
            let _ = ops;
            Ok(())
        }
        #[cfg(windows)]
        {
            for op in ops {
                let status = match op {
                    RegOp::SetValue { key, name, value, kind } => {
                        let vtype = match kind {
                            RegKind::Sz => "REG_SZ",
                            RegKind::ExpandSz => "REG_EXPAND_SZ",
                            RegKind::Dword => "REG_DWORD",
                        };
                        let mut args = vec!["add".to_string(), key.clone(), "/f".into(), "/t".into(), vtype.into()];
                        match name {
                            Some(n) => args.extend(["/v".into(), n.clone()]),
                            None => args.extend(["/ve".into()]),
                        }
                        args.push("/d".to_string());
                        args.push(value.clone());
                        std::process::Command::new("reg").args(&args).status()?
                    }
                    RegOp::DeleteKey { key } => std::process::Command::new("reg")
                        .args(["delete", key, "/f"])
                        .status()?,
                    RegOp::DeleteValue { key, name } => std::process::Command::new("reg")
                        .args(["delete", key, "/v", name, "/f"])
                        .status()?,
                };
                if !status.success() {
                    return Err(std::io::Error::other(format!("reg.exe failed for {op:?}")));
                }
            }
            Ok(())
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
    fn assoc_offers_progid_without_hijacking_default() {
        let ops = assoc_ops(exe());
        // We write an OpenWithProgids entry...
        assert_eq!(
            find_set(&ops, r"HKCU\Software\Classes\.zip\OpenWithProgids", Some("ZipNest.zip")),
            Some("")
        );
        // ...and never the extension's default value.
        assert!(find_set(&ops, r"HKCU\Software\Classes\.zip", None).is_none());
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

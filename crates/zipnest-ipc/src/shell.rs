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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ShellOptions {
    pub associate: bool,
    pub context_menu: bool,
    /// Which extensions to claim as default. Empty means "all supported", which
    /// is what the installer and the first-run bootstrap ask for; the Settings
    /// dialog sends the ticked subset.
    #[serde(default)]
    pub extensions: Vec<String>,
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
    /// Remove a value only while it still holds `expect`. Unregistering must
    /// never clobber a handler the user picked after we wrote ours.
    DeleteValueIfEquals {
        key: String,
        /// `None` targets the key's default value.
        name: Option<String>,
        expect: String,
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

/// `HKCU\Software\ZipNest\Capabilities` — the block Windows reads to list
/// ZipNest under 设置 → 应用 → 默认应用 with its own "设为默认值" button.
const CAPABILITIES: &str = r"HKCU\Software\ZipNest\Capabilities";
const REGISTERED_APPS: &str = r"HKCU\Software\RegisteredApplications";
const REGISTERED_APP_NAME: &str = "ZipNest";

/// File name behind `...\Classes\Applications\<name>`. A constant, not derived
/// from the running exe, so [`assoc_ops`] and [`assoc_removals`] can never
/// disagree about which key they own.
const APP_EXE_NAME: &str = "zipnest.exe";

fn prog_id(ext: &str) -> String {
    format!("ZipNest.{ext}")
}

/// Canonicalise a user-chosen extension list: lower case, only supported
/// extensions, no duplicates, in [`SUPPORTED_EXTENSIONS`] order.
///
/// Used on the way in (settings) and on the way out (registry ops), so a
/// hand-edited `settings.json` can never register something we do not ship a
/// handler for.
pub fn normalise_extensions(list: &[String]) -> Vec<String> {
    SUPPORTED_EXTENSIONS
        .iter()
        .filter(|ext| {
            list.iter()
                .any(|want| want.trim().trim_start_matches('.').eq_ignore_ascii_case(ext))
        })
        .map(|ext| (*ext).to_string())
        .collect()
}

/// `HKCU\Software\Classes\Applications\zipnest.exe` — what puts ZipNest in the
/// "打开方式" list as "ZipNest" instead of as a bare path.
fn app_key() -> String {
    format!(r"{CLASSES}\Applications\{APP_EXE_NAME}")
}

/// `HKCU\...\Explorer\FileExts\.<ext>\UserChoice` — Windows' per-extension
/// locked-in choice.
pub fn user_choice_key(ext: &str) -> String {
    format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}\UserChoice")
}

/// Deep link to the Windows page where the user can hand ZipNest the archive
/// extensions. Windows 11 honours `registeredAppUser`; Windows 10 opens the
/// same Default apps page and ignores the extra parameter.
pub const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps?registeredAppUser=ZipNest";

/// Open Windows' own "set default programs" page **scoped to ZipNest** — the
/// one-click path Bandizip and friends use.
///
/// Best-effort by design. `LaunchAdvancedAssociationUI` shows the classic *Set
/// Program Associations* page with ZipNest preselected, which is the only way
/// Windows lets the default change once a `UserChoice` already belongs to
/// another program (we cannot write or delete that key — see
/// [`user_choice_blocks`] — but the user can replace it there in one click).
///
/// It is also unreliable: measured on Windows 10 build 28020, it returns
/// `E_INVALIDARG` (0x80070057) for *every* application name tried, including
/// machine-wide ones such as "Microsoft Edge" — that build has no working
/// classic page left. When it blocks, it blocks until the page is closed, so
/// callers must not wait for it on the UI thread. Callers should therefore treat
/// an `Err` as "use the `ms-settings:` page instead" rather than as a failure to
/// report.
///
/// Requires the `RegisteredApplications` + `Capabilities` entries that
/// [`assoc_ops`] writes, without which the page would not know ZipNest.
///
/// Returns `Ok(())` on success, or `Err(reason)` with the raw HRESULT so the
/// caller can log it, fall back to [`DEFAULT_APPS_URI`] or to the plain Settings
/// page. Call it off the UI thread: the Control Panel page it opens is modal in
/// some builds.
pub fn launch_default_apps_ui() -> Result<(), String> {
    #[cfg(windows)]
    {
        win32::launch_advanced_association_ui()
    }
    #[cfg(not(windows))]
    {
        Err("not Windows".to_string())
    }
}

/// Does the extension's `UserChoice` keep us from being the default?
///
/// Once the user picks an app in the "打开方式" dialog, Windows writes
/// `UserChoice` with a hash and a **Deny SetValue** ACE for the user. The key
/// cannot be written and cannot even be deleted (measured: `reg delete /f` →
/// access denied). Everything [`assoc_ops`] writes is ignored for that one
/// extension, so the honest answer is "report it and send the user to the
/// system page" — never a forged hash.
pub fn user_choice_blocks(ext: &str, user_choice: Option<&str>) -> bool {
    match user_choice {
        Some(pid) => !pid.eq_ignore_ascii_case(&prog_id(ext)),
        None => false,
    }
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
///
/// Three more blocks are written so Windows treats ZipNest as a real handler
/// rather than an anonymous path:
/// - `...\Classes\Applications\zipnest.exe` — the "打开方式" entry and the
///   name Explorer shows next to it;
/// - `HKCU\Software\ZipNest\Capabilities` + `RegisteredApplications` — the
///   registration that makes ZipNest appear in 设置 → 应用 → 默认应用, which
///   is the only place a user can hand it an extension whose `UserChoice`
///   already belongs to another program.
pub fn assoc_ops(exe: &std::path::Path) -> Vec<RegOp> {
    assoc_ops_for(exe, &SUPPORTED_EXTENSIONS)
}

/// Same, for a chosen subset of extensions.
///
/// The per-extension selection exists because "all archives" is not what
/// everybody wants: someone may keep `.tar` with their dev tools and still want
/// `.zip` to open here. The unregister direction always clears all nine, so a
/// subset can never leave a stray handler behind.
pub fn assoc_ops_for(exe: &std::path::Path, exts: &[&str]) -> Vec<RegOp> {
    let icon = format!("{},0", quoted(exe));
    let cmd = open_command(exe);
    let app = app_key();
    let mut ops = Vec::new();
    for ext in exts {
        let ext = *ext;
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
            cmd.clone(),
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
        // ... and list the extension under the application entry.
        ops.push(RegOp::set(
            format!(r"{app}\SupportedTypes\.{ext}"),
            None,
            "",
        ));
    }
    // Extensions the caller did NOT tick: release the ones we still hold, so
    // unticking a box really gives that extension back (only while the recorded
    // default is ours -- never someone else's value). ZipNest stays listed in
    // "打开方式" and in 默认应用 for them, which is how the user can come back.
    for ext in SUPPORTED_EXTENSIONS {
        if exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            continue;
        }
        ops.push(RegOp::DeleteValueIfEquals {
            key: format!(r"{CLASSES}\.{ext}"),
            name: None,
            expect: prog_id(ext),
        });
    }
    // The application entry itself: one key, shared by every extension.
    ops.push(RegOp::set(format!(r"{app}\FriendlyAppName"), None, "ZipNest"));
    ops.push(RegOp::set(format!(r"{app}\shell\open\command"), None, cmd));
    // Capabilities + the RegisteredApplications pointer that exposes them.
    // Always all nine: this is the list Windows shows in "默认应用" and in the
    // "open with" dialog, i.e. the way *back* to ZipNest even for an extension
    // the user did not tick here.
    ops.push(RegOp::set(CAPABILITIES, None, REGISTERED_APP_NAME));
    ops.push(RegOp::set(
        format!(r"{CAPABILITIES}\ApplicationName"),
        None,
        REGISTERED_APP_NAME,
    ));
    ops.push(RegOp::set(
        format!(r"{CAPABILITIES}\ApplicationDescription"),
        None,
        "ZipNest archive manager",
    ));
    for ext in SUPPORTED_EXTENSIONS {
        ops.push(RegOp::set(
            format!(r"{CAPABILITIES}\FileAssociations\.{ext}"),
            None,
            prog_id(ext),
        ));
    }
    ops.push(RegOp::set(
        REGISTERED_APPS,
        Some(REGISTERED_APP_NAME),
        r"Software\ZipNest\Capabilities",
    ));
    ops
}

/// Undo [`assoc_ops`]: drop each ProgID tree, its `OpenWithProgids` entry, the
/// "we are the default handler" value we wrote, the application entry and the
/// capability registration.
///
/// The extension's default value is only cleared while it still points at our
/// ProgID: leaving it behind would point the extension at a handler that no
/// longer exists (double-click breaks, icon goes blank), and clearing someone
/// else's value would be worse. Clearing ours hands the extension back to the
/// next registration in the chain (an `HKLM` handler, `OpenWithProgids`, or the
/// user's `UserChoice`).
pub fn assoc_removals() -> Vec<RegOp> {
    let mut ops = Vec::new();
    for ext in SUPPORTED_EXTENSIONS {
        let pid = prog_id(ext);
        ops.push(RegOp::DeleteValueIfEquals {
            key: format!(r"{CLASSES}\.{ext}"),
            name: None,
            expect: pid.clone(),
        });
        ops.push(RegOp::DeleteKey { key: format!(r"{CLASSES}\{pid}") });
        ops.push(RegOp::DeleteValue {
            key: format!(r"{CLASSES}\.{ext}\OpenWithProgids"),
            name: pid,
        });
    }
    ops.push(RegOp::DeleteKey { key: app_key() });
    ops.push(RegOp::DeleteKey { key: r"HKCU\Software\ZipNest".to_string() });
    ops.push(RegOp::DeleteValue {
        key: REGISTERED_APPS.to_string(),
        name: REGISTERED_APP_NAME.to_string(),
    });
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

/// Context-menu ops for one target: a top-level "ZipNest" verb whose default
/// action is open, plus a submenu "添加到压缩包…" that passes `--add "%1"`.
/// Files and folders get `%1`; the desktop/folder background gets `%V`.
pub fn menu_ops(target: MenuTarget, exe: &std::path::Path) -> Vec<RegOp> {
    let (base, arg) = match target {
        MenuTarget::File => (r"*\shell\ZipNest", "\"%1\""),
        MenuTarget::Directory => (r"Directory\shell\ZipNest", "\"%1\""),
        MenuTarget::Background => (r"Directory\Background\shell\ZipNest", "\"%V\""),
    };
    let key = format!(r"{CLASSES}\{base}");
    let exe_q = quoted(exe);
    vec![
        // Top-level verb + default (open) command.
        RegOp::set(key.clone(), Some("MUIVerb"), "ZipNest"),
        RegOp::set(key.clone(), Some("Icon"), format!("{},0", exe_q)),
        RegOp::set(format!(r"{key}\command"), None, format!("{exe_q} {arg}")),
        // Submenu entry: "添加到压缩包…".
        RegOp::set(
            format!(r"{key}\shell\add"),
            Some("MUIVerb"),
            "添加到压缩包…",
        ),
        RegOp::set(format!(r"{key}\shell\add\command"), None, format!("{exe_q} --add {arg}")),
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

/// Undo every context-menu entry (top-level verb tree is deleted recursively).
pub fn context_menu_removals() -> Vec<RegOp> {
    [MenuTarget::File, MenuTarget::Directory, MenuTarget::Background]
        .iter()
        .map(|t| RegOp::DeleteKey { key: menu_key(*t) })
        .collect()
}

/// Outcome of a (possibly partial) integration update. `warnings` holds one
/// i18n key per component the OS denied, so a hardened machine blocks only
/// that piece instead of the whole save. `blocked` lists the extensions whose
/// `UserChoice` belongs to another program — the registry writes succeeded,
/// but Windows still opens those with the other handler until the user changes
/// it in the system Default apps page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellRegisterResult {
    pub associate: bool,
    pub context_menu: bool,
    pub warnings: Vec<String>,
    pub blocked: Vec<String>,
}

/// Applies generated ops. Implemented for real by [`WindowsRegistry`] and by
/// a recorder in tests.
pub trait ShellApplier {
    fn run(&self, ops: &[RegOp]) -> std::io::Result<()>;
}

/// Reads the state of Windows' own choice for an extension. Separate from
/// [`ShellApplier`] because it is a query, and because tests want to answer it
/// without touching the registry.
pub trait AssocProbe {
    /// The `UserChoice` ProgID locked in for `ext`. `None` means "no choice
    /// recorded" (our registration wins) or "unreadable".
    fn user_choice_progid(&self, ext: &str) -> Option<String>;

    /// The ProgID recorded as the extension's default under
    /// `HKCU\Software\Classes\.<ext>`, i.e. what we last wrote there.
    fn classes_default(&self, ext: &str) -> Option<String>;
}

/// One row of the per-extension table the Settings dialog shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssocState {
    pub ext: String,
    /// Our ProgID is still the recorded default in `HKCU\Software\Classes`.
    pub registered: bool,
    /// Windows has locked this extension to someone else (`UserChoice`). Only
    /// the user can change that, from the OS dialog or Settings — which is what
    /// the per-row button opens.
    pub blocked_by: Option<String>,
}

impl AssocState {
    /// Is ZipNest what Windows will actually use for this extension?
    pub fn ours(&self) -> bool {
        self.registered && self.blocked_by.is_none()
    }

    /// Does this row need the user to decide something in the OS?
    pub fn needs_user_choice(&self) -> bool {
        self.blocked_by.is_some()
    }
}

/// The full per-extension picture, in [`SUPPORTED_EXTENSIONS`] order.
///
/// This is what makes the Settings dialog honest: a tick in a checkbox means
/// "asked for", while this says what Windows actually resolved.
pub fn assoc_states(probe: &dyn AssocProbe) -> Vec<AssocState> {
    SUPPORTED_EXTENSIONS
        .iter()
        .map(|ext| {
            let registered = probe
                .classes_default(ext)
                .map(|v| v.eq_ignore_ascii_case(&prog_id(ext)))
                .unwrap_or(false);
            let choice = probe.user_choice_progid(ext);
            let blocked_by = if user_choice_blocks(ext, choice.as_deref()) {
                choice
            } else {
                None
            };
            AssocState {
                ext: (*ext).to_string(),
                registered,
                blocked_by,
            }
        })
        .collect()
}

/// Open the system's **"你要如何打开此文件？"** dialog for one extension, which
/// is the only route that changes a `UserChoice` that belongs to another program.
///
/// There is no API to set the default handler; Windows writes the protected
/// `UserChoice` (hash and all) only when *the user* picks an app and ticks
/// "始终". `SHOpenWithDialog` is exactly that dialog, so this is what Bandizip
/// and friends do for the extensions they cannot claim directly: list them,
/// and let the user confirm each one in the OS.
///
/// A zero-byte sample file is created for the extension so the dialog has
/// something to be about; `OAIF_EXEC` is deliberately not set, so confirming
/// does not launch an archiver on it. Returns `Err` with the reason when the
/// shell refuses (the caller can fall back to `ms-settings:defaultapps`).
pub fn open_with_dialog_for(ext: &str) -> Result<(), String> {
    if !SUPPORTED_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
        return Err(format!("unsupported extension: {ext}"));
    }
    #[cfg(windows)]
    {
        win32::open_with_dialog(ext)
    }
    #[cfg(not(windows))]
    {
        Err("not Windows".to_string())
    }
}

/// Bullet-proof fallback for [`open_with_dialog_for`]: the classic
/// `OpenAs_RunDLL` entry point, which predates the documented API and still
/// exists. Used when `SHOpenWithDialog` is unavailable (it needs shell32 6.0+).
pub fn open_with_rundll_command(ext: &str) -> Option<(String, Vec<String>)> {
    let sample = sample_file_for(ext)?;
    Some((
        "rundll32.exe".to_string(),
        vec![
            "shell32.dll,OpenAs_RunDLL".to_string(),
            sample.to_string_lossy().into_owned(),
        ],
    ))
}

/// Path of the (created) zero-byte sample file the OS dialog is shown for.
fn sample_file_for(ext: &str) -> Option<std::path::PathBuf> {
    if !SUPPORTED_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
        return None;
    }
    let dir = std::env::temp_dir().join("zipnest-assoc");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("sample.{}", ext.to_ascii_lowercase()));
    if !path.exists() {
        std::fs::write(&path, b"").ok()?;
    }
    Some(path)
}

/// Extensions where Windows' locked-in choice names somebody else's handler,
/// so [`assoc_ops`] cannot take effect no matter how it is written.
pub fn blocked_extensions(probe: &dyn AssocProbe) -> Vec<String> {
    SUPPORTED_EXTENSIONS
        .iter()
        .filter(|ext| user_choice_blocks(ext, probe.user_choice_progid(ext).as_deref()))
        .map(|ext| (*ext).to_string())
        .collect()
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
    const KEY_QUERY_VALUE: u32 = 0x0001;
    const KEY_WOW64_64KEY: u32 = 0x0100;
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const REG_DWORD: u32 = 4;

    // --- COM, for the "set as default" page -------------------------------
    // {1968106D-F3B5-44CF-890E-116FCB9ECEF1}
    const CLSID_APPLICATION_ASSOCIATION_REGISTRATION_UI: Guid = Guid {
        data1: 0x1968_106d,
        data2: 0xf3b5,
        data3: 0x44cf,
        data4: [0x89, 0x0e, 0x11, 0x6f, 0xcb, 0x9e, 0xce, 0xf1],
    };
    // {1F76A169-F994-40AC-8FC8-0959E8874710}
    const IID_IAPPLICATION_ASSOCIATION_REGISTRATION_UI: Guid = Guid {
        data1: 0x1f76_a169,
        data2: 0xf994,
        data3: 0x40ac,
        data4: [0x8f, 0xc8, 0x09, 0x59, 0xe8, 0x87, 0x47, 0x10],
    };
    const CLSCTX_INPROC_SERVER: u32 = 1;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    /// `CoInitializeEx` when COM was already initialized on this thread.
    const S_FALSE: i32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// Minimal vtable of `IApplicationAssociationRegistrationUI`: three
    /// `IUnknown` slots, then the one method we call. The repository does the
    /// same thing for the 7-Zip interfaces in `archive-core::com`, and the
    /// layouts are checked against the SDK headers there.
    #[repr(C)]
    struct ApplicationAssociationRegistrationUiVt {
        query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        launch_advanced_association_ui: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            clsid: *const Guid,
            outer: *mut c_void,
            clsctx: u32,
            iid: *const Guid,
            out: *mut *mut c_void,
        ) -> i32;
    }

    /// `OPENASINFO` from shellapi.h.
    #[repr(C)]
    struct OpenAsInfo {
        file: *const u16,
        class: *const u16,
        flags: u32,
    }

    /// The "始终使用此应用" checkbox is what makes Windows write the protected
    /// `UserChoice`; without `OAIF_REGISTER_EXT` the choice is not recorded.
    /// `OAIF_EXEC` is deliberately absent: we do not want the chosen program
    /// started on our empty sample file.
    const OAIF_ALLOW_REGISTRATION: u32 = 0x0000_0001;
    const OAIF_REGISTER_EXT: u32 = 0x0000_0002;

    #[link(name = "shell32")]
    extern "system" {
        fn SHOpenWithDialog(hwnd: isize, info: *const OpenAsInfo) -> i32;
    }

    /// Show the system "你要如何打开此文件？" dialog for
    /// `<temp>\zipnest-assoc\sample.<ext>`, where the user can pick ZipNest and
    /// tick "始终". That is the only way the protected `UserChoice` changes.
    pub fn open_with_dialog(ext: &str) -> Result<(), String> {
        let sample = super::sample_file_for(ext)
            .ok_or_else(|| format!("could not create a sample file for .{ext}"))?;
        let mut file: Vec<u16> = sample.to_string_lossy().encode_utf16().collect();
        file.push(0);
        let info = OpenAsInfo {
            file: file.as_ptr(),
            class: std::ptr::null(),
            flags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT,
        };
        // A null owner window is fine; the dialog is modal to the desktop.
        let hr = unsafe { SHOpenWithDialog(0, &info) };
        if hr < 0 {
            Err(format!("SHOpenWithDialog failed: 0x{:08X}", hr as u32))
        } else {
            Ok(())
        }
    }

    /// Open the per-app "Set program associations" page. `Err` carries the raw
    /// HRESULT (hex) so the caller can report or log what the OS said.
    pub fn launch_advanced_association_ui() -> Result<(), String> {
        launch_advanced_association_ui_for(super::REGISTERED_APP_NAME)
    }

    /// Same, for an arbitrary registered-application name. Split out so the
    /// smoke test can check the OS behaviour against names it knows are
    /// registered machine-wide.
    pub fn launch_advanced_association_ui_for(app_name: &str) -> Result<(), String> {
        // The UI thread may already be STA (winit); S_FALSE means "already
        // initialized, do not uninitialize on the way out".
        let hr_init = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED) };
        let we_initialized = hr_init >= 0 && hr_init != S_FALSE;
        if hr_init < 0 {
            return Err(format!("CoInitializeEx failed: 0x{:08X}", hr_init as u32));
        }

        let mut raw: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            CoCreateInstance(
                &CLSID_APPLICATION_ASSOCIATION_REGISTRATION_UI,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IAPPLICATION_ASSOCIATION_REGISTRATION_UI,
                &mut raw,
            )
        };
        if hr < 0 || raw.is_null() {
            if we_initialized {
                unsafe { CoUninitialize() };
            }
            return Err(format!(
                "CoCreateInstance(ApplicationAssociationRegistrationUI) failed: 0x{:08X}",
                hr as u32
            ));
        }

        let vt = unsafe { *(raw as *mut *const ApplicationAssociationRegistrationUiVt) };
        let mut name: Vec<u16> = app_name.encode_utf16().collect();
        name.push(0);
        let hr = unsafe { ((*vt).launch_advanced_association_ui)(raw, name.as_ptr()) };
        unsafe {
            ((*vt).release)(raw);
        }
        if we_initialized {
            unsafe { CoUninitialize() };
        }
        if hr < 0 {
            Err(format!("LaunchAdvancedAssociationUI failed: 0x{:08X}", hr as u32))
        } else {
            Ok(())
        }
    }

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
        fn RegOpenKeyExW(
            hkey: isize,
            subkey: *const u16,
            options: u32,
            sam: u32,
            result: *mut isize,
        ) -> i32;
        fn RegQueryValueExW(
            hkey: isize,
            name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            len: *mut u32,
        ) -> i32;
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

    /// Read a `REG_SZ` value; `None` when the key/value is missing or holds a
    /// different type. Used by [`RegOp::DeleteValueIfEquals`].
    fn read_value(key: &str, name: Option<&str>) -> Option<String> {
        let (root, sub) = split(key);
        let mut sub_w = wide(&sub);
        sub_w.push(0);
        let mut hkey = 0isize;
        let rc = unsafe {
            RegOpenKeyExW(
                root,
                sub_w.as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_WOW64_64KEY,
                &mut hkey,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let name_w: Vec<u16> = match name {
            Some(n) => {
                let mut v = wide(n);
                v.push(0);
                v
            }
            None => vec![0],
        };
        let mut kind = 0u32;
        let mut len = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                hkey,
                name_w.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if rc != ERROR_SUCCESS || kind != REG_SZ || len == 0 {
            unsafe {
                RegCloseKey(hkey);
            }
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let rc = unsafe {
            RegQueryValueExW(
                hkey,
                name_w.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        if rc != ERROR_SUCCESS {
            return None;
        }
        let units: Vec<u16> = buf
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
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

    /// Read a `REG_SZ` value for callers outside this module (the [`AssocProbe`]
    /// implementation, which reads Windows' `UserChoice`).
    pub fn read_sz(key: &str, name: Option<&str>) -> Option<String> {
        read_value(key, name)
    }

    pub fn apply(ops: &[RegOp]) -> std::io::Result<()> {
        for op in ops {
            let rc = match op {
                RegOp::SetValue { key, name, value, kind } => set_value(key, name.as_deref(), value, *kind),
                RegOp::DeleteKey { key } => delete_key(key),
                RegOp::DeleteValue { key, name } => delete_value(key, name),
                RegOp::DeleteValueIfEquals { key, name, expect } => {
                    match read_value(key, name.as_deref()) {
                        Some(current) if current.eq_ignore_ascii_case(expect) => {
                            delete_value(key, name.as_deref().unwrap_or(""))
                        }
                        // Missing already, a different type, or someone else's
                        // handler: nothing of ours to remove.
                        _ => ERROR_SUCCESS,
                    }
                }
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

impl AssocProbe for WindowsRegistry {
    fn user_choice_progid(&self, ext: &str) -> Option<String> {
        #[cfg(not(windows))]
        {
            let _ = ext;
            None
        }
        #[cfg(windows)]
        {
            win32::read_sz(&user_choice_key(ext), Some("ProgId"))
        }
    }

    fn classes_default(&self, ext: &str) -> Option<String> {
        #[cfg(not(windows))]
        {
            let _ = ext;
            None
        }
        #[cfg(windows)]
        {
            win32::read_sz(&format!(r"{CLASSES}\.{ext}"), None)
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
    fn assoc_removals_clear_only_our_extension_default() {
        // The default value we wrote must be dropped, and dropped
        // *conditionally*: whatever the user picks after enabling ZipNest has
        // to survive an unregister.
        let removals = assoc_removals();
        for ext in SUPPORTED_EXTENSIONS {
            let pid = prog_id(ext);
            let key = format!(r"HKCU\Software\Classes\.{ext}");
            let default_at = removals
                .iter()
                .position(|o| {
                    matches!(
                        o,
                        RegOp::DeleteValueIfEquals { key: k, name: None, expect }
                            if k == &key && expect == &pid
                    )
                })
                .unwrap_or_else(|| panic!("extension default not cleared for .{ext}"));
            let tree_at = removals
                .iter()
                .position(|o| matches!(o, RegOp::DeleteKey { key: k } if k == &format!(r"HKCU\Software\Classes\{pid}")))
                .expect("progid tree removal");
            assert!(
                default_at < tree_at,
                "the default value must be cleared before the ProgID tree goes away"
            );
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

    #[test]
    fn assoc_registers_the_application_entry() {
        // Without `...\Classes\Applications\zipnest.exe` the "打开方式" list
        // shows a bare path instead of "ZipNest".
        let ops = assoc_ops(exe());
        assert_eq!(
            find_set(&ops, r"HKCU\Software\Classes\Applications\zipnest.exe\shell\open\command", None),
            Some(r#""C:\Program Files\ZipNest\ZipNest.exe" "%1""#)
        );
        assert_eq!(
            find_set(&ops, r"HKCU\Software\Classes\Applications\zipnest.exe\FriendlyAppName", None),
            Some("ZipNest")
        );
        for ext in SUPPORTED_EXTENSIONS {
            let key = format!(r"HKCU\Software\Classes\Applications\zipnest.exe\SupportedTypes\.{ext}");
            assert_eq!(find_set(&ops, &key, None), Some(""), "missing SupportedTypes for .{ext}");
        }
    }

    #[test]
    fn assoc_registers_capabilities_so_windows_lists_us() {
        // `RegisteredApplications` + a Capabilities block is what puts ZipNest
        // in 设置 → 应用 → 默认应用, the only place a locked-out extension can
        // be handed back to us.
        let ops = assoc_ops(exe());
        assert_eq!(
            find_set(&ops, r"HKCU\Software\RegisteredApplications", Some("ZipNest")),
            Some(r"Software\ZipNest\Capabilities")
        );
        assert_eq!(
            find_set(&ops, r"HKCU\Software\ZipNest\Capabilities", None),
            Some("ZipNest")
        );
        assert_eq!(
            find_set(&ops, r"HKCU\Software\ZipNest\Capabilities\ApplicationName", None),
            Some("ZipNest")
        );
        for ext in SUPPORTED_EXTENSIONS {
            let key = format!(r"HKCU\Software\ZipNest\Capabilities\FileAssociations\.{ext}");
            assert_eq!(
                find_set(&ops, &key, None),
                Some(prog_id(ext).as_str()),
                "capability missing for .{ext}"
            );
        }
    }

    #[test]
    fn assoc_removals_drop_the_application_and_capability_keys() {
        let removals = assoc_removals();
        for key in [
            r"HKCU\Software\Classes\Applications\zipnest.exe",
            r"HKCU\Software\ZipNest",
        ] {
            assert!(
                removals.iter().any(|o| matches!(o, RegOp::DeleteKey { key: k } if k == key)),
                "removal missing for {key}"
            );
        }
        assert!(removals.iter().any(|o| matches!(
            o,
            RegOp::DeleteValue { key, name }
                if key == r"HKCU\Software\RegisteredApplications" && name == "ZipNest"
        )));
    }

    #[test]
    fn removals_never_touch_another_program() {
        // Every op must be scoped to a key this app owns: uninstalling must
        // never clear a ProgrID the user picked, or another archiver's entry.
        let removals = assoc_removals();
        for op in &removals {
            let key = match op {
                RegOp::SetValue { key, .. }
                | RegOp::DeleteKey { key }
                | RegOp::DeleteValue { key, .. }
                | RegOp::DeleteValueIfEquals { key, .. } => key,
            };
            let owned = key.contains(r"\ZipNest")
                || key.contains(r"\ZipNest.")
                || key.contains(r"\Applications")
                || key.starts_with(r"HKCU\Software\Classes\.")
                || key == r"HKCU\Software\RegisteredApplications";
            assert!(owned, "removal reaches outside our keys: {key}");
        }
    }

    fn probe_with(choice: &'static str, exts: &'static [&'static str]) -> impl AssocProbe {
        struct Probe(&'static str, &'static [&'static str]);
        impl AssocProbe for Probe {
            fn user_choice_progid(&self, ext: &str) -> Option<String> {
                self.1.contains(&ext).then(|| self.0.to_string())
            }
            fn classes_default(&self, ext: &str) -> Option<String> {
                Some(format!("ZipNest.{ext}"))
            }
        }
        Probe(choice, exts)
    }

    #[test]
    fn the_per_extension_table_tells_ours_from_someone_elses() {
        // Registered everywhere; Windows has locked .zip and .rar to Bandizip.
        let probe = probe_with("Bandizip.zip", &["zip", "rar"]);
        let states = assoc_states(&probe);
        assert_eq!(states.len(), SUPPORTED_EXTENSIONS.len(), "one row per extension");
        let zip = states.iter().find(|s| s.ext == "zip").unwrap();
        assert!(zip.registered, "our Classes value is still in place");
        assert!(!zip.ours(), "but Windows will not use it");
        assert_eq!(zip.blocked_by.as_deref(), Some("Bandizip.zip"));
        // A row nobody else claimed is simply ours.
        let seven = states.iter().find(|s| s.ext == "7z").unwrap();
        assert!(seven.ours() && !seven.needs_user_choice());
    }

    #[test]
    fn a_row_with_no_registration_is_not_ours_even_when_unclaimed() {
        // "Not blocked" is not the same as "ours": with the integration switched
        // off there is no Classes value, and the table has to say so.
        struct Nothing;
        impl AssocProbe for Nothing {
            fn user_choice_progid(&self, _ext: &str) -> Option<String> {
                None
            }
            fn classes_default(&self, _ext: &str) -> Option<String> {
                None
            }
        }
        let states = assoc_states(&Nothing);
        assert!(states.iter().all(|s| !s.registered && !s.ours()));
        assert!(states.iter().all(|s| !s.needs_user_choice()));
    }

    #[test]
    fn only_archive_extensions_get_a_dialog_sample() {
        // The dialog needs a file of that extension; anything else is refused
        // rather than creating junk in %TEMP%.
        assert!(sample_file_for("zip").is_some());
        let sample = sample_file_for("zip").unwrap();
        assert_eq!(sample.extension().unwrap(), "zip");
        assert!(sample.exists(), "the sample file is created on demand");
        assert!(sample_file_for("exe").is_none());
        assert!(sample_file_for("../evil").is_none());
        assert!(open_with_rundll_command("7z").is_some());
        assert!(open_with_rundll_command("exe").is_none());
    }

    #[test]
    fn a_subset_registration_only_touches_the_ticked_extensions() {
        // The whole point of per-extension ticks: .zip yes, .rar untouched.
        let ops = assoc_ops_for(exe(), &["zip"]);
        assert!(ops.iter().any(|o| matches!(
            o,
            RegOp::SetValue { key, name: None, value, .. }
                if key == r"HKCU\Software\Classes\.zip" && value == "ZipNest.zip"
        )));
        assert!(
            !ops.iter().any(|o| matches!(o, RegOp::SetValue { key, .. } if key == r"HKCU\Software\Classes\.rar")),
            "an unticked extension must not be claimed"
        );
        // ... while the capabilities list still mentions all nine, so the user
        // can find ZipNest in "默认应用" for any of them.
        for ext in SUPPORTED_EXTENSIONS {
            let key = format!(r"HKCU\Software\ZipNest\Capabilities\FileAssociations\.{ext}");
            assert!(ops.iter().any(|o| matches!(o, RegOp::SetValue { key: k, .. } if k == &key)));
        }
    }

    #[test]
    fn user_choice_blocks_only_someone_elses_handler() {
        // No choice at all: our Classes default wins, nothing is blocked.
        assert!(!user_choice_blocks("zip", None));
        // Our own ProgID: no conflict, whatever the casing.
        assert!(!user_choice_blocks("zip", Some("ZipNest.zip")));
        assert!(!user_choice_blocks("zip", Some("zipnest.ZIP")));
        // Windows' built-in zip folder, or any other archiver: we lose.
        assert!(user_choice_blocks("zip", Some("CompressedFolder")));
        assert!(user_choice_blocks("7z", Some("7-Zip.7z")));
        // A choice recorded for a *different* extension is irrelevant.
        assert!(!user_choice_blocks("rar", None));
    }

    #[test]
    fn blocked_extensions_reports_the_locked_out_ones() {
        let probe = probe_with("Bandizip.zip", &["zip", "rar"]);
        assert_eq!(blocked_extensions(&probe), vec!["zip".to_string(), "rar".to_string()]);

        let none = probe_with("ZipNest.zip", &[]);
        assert!(blocked_extensions(&none).is_empty());

        // Our own ProgIDs everywhere: a fully claimed machine reports nothing.
        let ours = probe_with("ZipNest.7z", &["7z"]);
        assert!(!blocked_extensions(&ours).contains(&"7z".to_string()));
    }

    #[test]
    fn user_choice_key_and_default_apps_uri_are_the_documented_ones() {
        assert_eq!(
            user_choice_key("zip"),
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.zip\UserChoice"
        );
        // Windows 11 deep-links to the per-app page by capability name.
        assert_eq!(
            DEFAULT_APPS_URI,
            "ms-settings:defaultapps?registeredAppUser=ZipNest"
        );
    }

    #[test]
    fn real_registry_probe_classifies_the_environment_consistently() {        // Smoke test against the machine's real state. On a machine with no
        // `UserChoice` for these extensions it reports nothing; the point is
        // that the live probe and the pure classifier never disagree.
        let probe = WindowsRegistry;
        let blocked = blocked_extensions(&probe);
        for ext in SUPPORTED_EXTENSIONS {
            let got = probe.user_choice_progid(ext);
            assert_eq!(
                blocked.contains(&ext.to_string()),
                user_choice_blocks(ext, got.as_deref()),
                "probe and classifier disagree for .{ext}"
            );
        }
    }

    /// The one-click "make ZipNest the default" path, against the real OS.
    ///
    /// `#[ignore]`d because it *opens a window* (Windows' "Set program
    /// associations" page, with ZipNest preselected) and therefore cannot run in
    /// CI. Run it by hand when touching the COM plumbing:
    ///
    /// ```text
    /// cargo test -p zipnest-ipc -- --ignored launch_default_apps_ui_opens
    /// ```
    ///
    /// Which registrations does the classic API accept on this machine?
    ///
    /// A hand-run diagnostic, not a test of our code: it calls
    /// `LaunchAdvancedAssociationUI` for a few names that are registered
    /// machine-wide (and one that is per-user only) with a 5 s cap each, and
    /// prints the verdict. A call that "times out" opened the modal page; a
    /// printed HRESULT was refused.
    ///
    /// Measured on this box (Windows 10 26H1, build 28020) while building
    /// v0.4.11: **every** name returns E_INVALIDARG (0x80070057), ours and
    /// machine-registered ones alike, with and without the installer's HKLM
    /// entry — the classic page is gone from that build. That is why the
    /// Settings dialog treats the call as best-effort and falls back to the
    /// `ms-settings:` page, which is also what Bandizip does on current Windows.
    /// Open the per-extension system dialog for real.
    ///
    /// `#[ignore]`d because it puts a modal **"你要如何打开此文件？"** window on the
    /// desktop and blocks until it is answered. Run it by hand when touching this
    /// path:
    ///
    /// ```text
    /// cargo test -p zipnest-ipc -- --ignored --nocapture open_with_dialog_for_zip
    /// ```
    ///
    /// The call runs on a worker thread and the test only waits a few seconds, so
    /// it never hangs: "still waiting" means the dialog is on screen. Close it
    /// with Esc when done.
    #[test]
    #[ignore = "opens a modal shell dialog; run by hand"]
    fn open_with_dialog_for_zip() {
        let sample = sample_file_for("zip").expect("sample file");
        println!(
            "  sample: {} ({} bytes)",
            sample.display(),
            std::fs::metadata(&sample).map(|m| m.len()).unwrap_or(0)
        );

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(super::open_with_dialog_for("zip"));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(8)) {
            Ok(Ok(())) => println!("  returned Ok immediately (dialog already dismissed?)"),
            Ok(Err(e)) => panic!("the shell refused the dialog: {e}"),
            Err(_) => println!("  still waiting after 8 s -> the dialog is open and modal"),
        }
    }

    #[test]
    #[ignore = "opens Control Panel windows; run by hand"]
    fn probe_which_registered_apps_the_api_accepts() {
        for name in [
            "Microsoft Edge",
            "Windows Photo Viewer",
            "MPC-BE",
            super::REGISTERED_APP_NAME,
        ] {
            let (tx, rx) = std::sync::mpsc::channel();
            let owned = name.to_string();
            std::thread::spawn(move || {
                let _ = tx.send(super::win32::launch_advanced_association_ui_for(&owned));
            });
            match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(Ok(())) => println!("  {name}: returned Ok"),
                Ok(Err(e)) => println!("  {name}: {e}"),
                Err(_) => println!("  {name}: still running after 5 s -> page opened"),
            }
        }
    }

    /// The one-click "make ZipNest the default" path, against the real OS.
    ///
    /// `#[ignore]`d because it *opens* Windows' "Set program associations" page
    /// (with ZipNest preselected) and that page is modal: the call blocks until
    /// the user closes it. Run it by hand when touching the COM plumbing:
    ///
    /// ```text
    /// cargo test -p zipnest-ipc -- --ignored --nocapture launch_default_apps_ui_opens
    /// ```
    ///
    /// It runs the call on a worker thread and only waits a few seconds, so the
    /// test never hangs: "still running after N s" IS the success signal (the
    /// page is on screen and waiting), while a printed HRESULT is the failure.
    /// Close the page the test opens.
    #[test]
    #[ignore = "opens a Control Panel window; run by hand"]
    fn launch_default_apps_ui_opens_the_system_page() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(launch_default_apps_ui());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(6)) {
            Ok(Ok(())) => println!("  returned Ok immediately (page may already have been open)"),
            Ok(Err(e)) => panic!("the OS refused the call: {e}"),
            Err(_) => println!(
                "  still waiting after 6 s -> the page opened and is modal.\n  \
                 Close it. (E_INVALIDARG here means HKLM\\SOFTWARE\\RegisteredApplications\n  \
                 has no ZipNest entry: only the installer writes that half.)"
            ),
        }
    }
}


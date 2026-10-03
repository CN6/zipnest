//! `zipnest_shell.dll` — ZipNest's entry in the Windows 11 **modern** context
//! menu ("添加到压缩包…" / "Add to archive…").
//!
//! One `IExplorerCommand` class, declared by the sparse package's manifest. The
//! handler holds no application logic: it collects the selection's file-system
//! paths and starts `zipnest.exe --add <paths>` once. The executable is found
//! beside this DLL only, so the package can point at the user's existing
//! install directory through `uap10:AllowExternalContent` — no HKLM, no shell
//! registry writes, no trusted certificate.
//!
//! Every COM entry point runs under `catch_unwind`: a panic must never unwind
//! into `explorer.exe` or the surrogate host.
#![cfg(windows)]
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{implement, Interface, Ref, BOOL, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOTIMPL, E_POINTER,
    HMODULE, S_FALSE, S_OK,
};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Com::{
    CoTaskMemFree, IAgileObject, IAgileObject_Impl, IBindCtx, IClassFactory, IClassFactory_Impl,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItem, IShellItemArray,
    ECF_DEFAULT, ECS_ENABLED, ECS_HIDDEN, SHStrDupW, SIGDN_FILESYSPATH,
};

/// `{9F2C4E17-6A3B-4D58-8E21-5B7C0D9A4F33}` — stable, never reused. It must
/// match `com:Class Id` and `desktop5:Verb Clsid` in the package manifest.
pub const CLSID_ZIPNEST_ADD: GUID = GUID::from_u128(0x9F2C4E17_6A3B_4D58_8E21_5B7C0D9A4F33);

const EXE_NAME: &str = "zipnest.exe";

/// Live command objects, live class factories and server locks; the DLL may
/// unload only at zero.
static REFERENCES: AtomicIsize = AtomicIsize::new(0);

// --- small helpers -------------------------------------------------------

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `SHStrDupW` into a COM-owned string the Shell frees with `CoTaskMemFree`.
fn co_string(text: &str) -> windows::core::Result<PWSTR> {
    let buffer = wide(text);
    unsafe { SHStrDupW(PCWSTR(buffer.as_ptr())) }
}

/// Take ownership of a COM string and free it.
fn take_co_string(text: PWSTR) -> String {
    let value = unsafe { text.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(text.0 as *const c_void)) };
    value
}

fn guard<T>(body: impl FnOnce() -> windows::core::Result<T>) -> windows::core::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(_) => Err(E_FAIL.into()),
    }
}

/// Full path of this DLL, from the module itself — never the registry,
/// `PATH`, or the current directory.
fn module_path() -> Option<PathBuf> {
    let mut module = HMODULE::default();
    let anchor = module_path as *const ();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(anchor as *const u16),
            &mut module,
        )
    }
    .ok()?;
    let mut buffer = vec![0u16; 32768];
    let length = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let full = String::from_utf16_lossy(&buffer[..length]);
    Some(PathBuf::from(full))
}

/// `…\<dir>\zipnest_shell.dll` → `…\<dir>\zipnest.exe`.
fn exe_from_dll(dll: &Path) -> PathBuf {
    dll.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(EXE_NAME)
}

fn exe_path() -> Option<PathBuf> {
    module_path().map(|dll| exe_from_dll(&dll))
}

// --- label language ------------------------------------------------------

/// Extract a top-level JSON string value without pulling in a parser; the
/// settings file holds no escapes in `language`.
fn json_string_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let rest = &text[text.find(&needle)? + needle.len()..];
    let rest = &rest[rest.find(':')? + 1..];
    let rest = &rest[rest.find('"')? + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The app's own UI language from `%APPDATA%\ZipNest\settings.json`.
fn configured_language() -> Option<String> {
    let appdata = std::env::var_os("APPDATA")?;
    let path = PathBuf::from(appdata).join("ZipNest").join("settings.json");
    let text = std::fs::read_to_string(path).ok()?;
    json_string_value(&text, "language")
}

/// `"system"`/missing follows the Windows UI language; an explicit app choice
/// wins. Mirrors the app: Chinese unless the user picked English.
fn is_chinese() -> bool {
    match configured_language().as_deref() {
        Some("en-US") => false,
        Some("zh-CN") => true,
        _ => {
            let langid = unsafe { GetUserDefaultUILanguage() };
            (langid & 0x03ff) == 0x04
        }
    }
}

fn label() -> &'static str {
    if is_chinese() {
        "添加到压缩包…"
    } else {
        "Add to archive…"
    }
}

// --- selection & launch --------------------------------------------------

fn item_path(item: &IShellItem) -> Option<String> {
    let raw = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
    Some(take_co_string(raw))
}

fn selection_paths(items: &IShellItemArray) -> Vec<String> {
    let count = unsafe { items.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|index| {
            let item = unsafe { items.GetItemAt(index) }.ok()?;
            item_path(&item)
        })
        .collect()
}

/// Quote one argument for a `CreateProcessW` command line (MSVC rules).
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{0b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('"');
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Backslashes before the closing quote must be doubled.
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

fn build_command_line(exe: &Path, paths: &[String]) -> String {
    let mut line = quote(&exe.display().to_string());
    line.push_str(" --add");
    for path in paths {
        line.push(' ');
        line.push_str(&quote(path));
    }
    line
}

fn launch(exe: &Path, paths: &[String]) -> windows::core::Result<()> {
    let application = wide(&exe.display().to_string());
    let mut command = wide(&build_command_line(exe, paths));
    let folder = wide(&exe.parent().unwrap_or_else(|| Path::new(".")).display().to_string());
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR(folder.as_ptr()),
            &startup,
            &mut process,
        )
    }?;
    unsafe {
        let _ = CloseHandle(process.hThread);
        let _ = CloseHandle(process.hProcess);
    }
    Ok(())
}

// --- COM objects ---------------------------------------------------------

#[implement(IExplorerCommand, IAgileObject)]
struct Command;

impl IAgileObject_Impl for Command_Impl {}

impl Command {
    fn new() -> Self {
        REFERENCES.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Command {
    fn drop(&mut self) {
        REFERENCES.fetch_sub(1, Ordering::SeqCst);
    }
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        guard(|| co_string(label()))
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        guard(|| {
            let exe = exe_path().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            co_string(&format!("{},0", exe.display()))
        })
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(CLSID_ZIPNEST_ADD)
    }

    fn GetState(&self, _items: Ref<IShellItemArray>, _slow: BOOL) -> windows::core::Result<u32> {
        guard(|| {
            // Hide the verb when there is no app to launch next to this DLL.
            if exe_path().is_some_and(|exe| exe.is_file()) {
                Ok(ECS_ENABLED.0 as u32)
            } else {
                Ok(ECS_HIDDEN.0 as u32)
            }
        })
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _bind: Ref<IBindCtx>) -> windows::core::Result<()> {
        guard(|| {
            let Some(items) = items.as_ref() else {
                return Ok(());
            };
            let paths = selection_paths(items);
            if paths.is_empty() {
                return Ok(());
            }
            let exe = exe_path().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            launch(&exe, &paths)
        })
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

#[implement(IClassFactory, IAgileObject)]
struct Factory;

impl IAgileObject_Impl for Factory_Impl {}

impl Factory {
    fn new() -> Self {
        REFERENCES.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Factory {
    fn drop(&mut self) {
        REFERENCES.fetch_sub(1, Ordering::SeqCst);
    }
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<windows::core::IUnknown>,
        riid: *const GUID,
        object: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        guard(|| {
            if object.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            unsafe { *object = std::ptr::null_mut() };
            if !outer.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let command: IExplorerCommand = Command::new().into();
            unsafe { command.query(riid, object) }.ok()
        })
    }

    fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
        if lock.as_bool() {
            REFERENCES.fetch_add(1, Ordering::SeqCst);
        } else {
            REFERENCES.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

// --- DLL exports ---------------------------------------------------------

/// # Safety
/// Called by COM with valid `rclsid`, `riid` and `ppv` pointers or null.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    let result = std::panic::catch_unwind(|| {
        if ppv.is_null() || rclsid.is_null() || riid.is_null() {
            return E_POINTER;
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if unsafe { *rclsid } != CLSID_ZIPNEST_ADD {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory::new().into();
        unsafe { factory.query(riid, ppv) }
    });
    result.unwrap_or(E_FAIL)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if REFERENCES.load(Ordering::SeqCst) <= 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_spaces_and_trailing_backslashes() {
        assert_eq!(quote(r"C:\a\b.zip"), r"C:\a\b.zip");
        assert_eq!(quote(r"C:\Program Files\a.zip"), r#""C:\Program Files\a.zip""#);
        // Nothing to quote → left bare (so a trailing backslash is harmless).
        assert_eq!(quote(r"C:\dir\"), r"C:\dir\");
        // Space *and* trailing backslash → quoted, backslashes doubled.
        assert_eq!(quote(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote("a\"b"), r#""a\"b""#);
    }

    #[test]
    fn command_line_uses_add_with_quoted_paths() {
        let exe = Path::new(r"C:\Program Files\ZipNest\zipnest.exe");
        let line = build_command_line(exe, &[r"C:\a b\one.zip".into(), r"C:\two.7z".into()]);
        assert_eq!(
            line,
            r#""C:\Program Files\ZipNest\zipnest.exe" --add "C:\a b\one.zip" C:\two.7z"#
        );
    }

    #[test]
    fn exe_is_found_beside_the_dll_only() {
        assert_eq!(
            exe_from_dll(Path::new(r"C:\Program Files\ZipNest\zipnest_shell.dll")),
            PathBuf::from(r"C:\Program Files\ZipNest\zipnest.exe")
        );
    }

    #[test]
    fn reads_the_language_field_from_settings_json() {
        let json = "{\n  \"language\": \"en-US\",\n  \"zoom\": 100\n}";
        assert_eq!(json_string_value(json, "language").as_deref(), Some("en-US"));
        assert_eq!(json_string_value("{}", "language"), None);
        assert_eq!(json_string_value(r#"{"language":"zh-CN"}"#, "language").as_deref(), Some("zh-CN"));
    }

    #[test]
    fn class_object_answers_only_for_our_clsid() {
        let mut factory: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { DllGetClassObject(&CLSID_ZIPNEST_ADD, &IClassFactory::IID, &mut factory) };
        assert_eq!(hr, S_OK);
        assert!(!factory.is_null());
        let factory = unsafe { IClassFactory::from_raw(factory) };
        assert_eq!(DllCanUnloadNow(), S_FALSE);
        let command: IExplorerCommand = unsafe { factory.CreateInstance(None) }.unwrap();
        assert_eq!(unsafe { command.GetCanonicalName() }.unwrap(), CLSID_ZIPNEST_ADD);
        drop(factory);
        drop(command);
        assert_eq!(DllCanUnloadNow(), S_OK);

        let unknown = GUID::from_u128(0x1234);
        let mut other: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe { DllGetClassObject(&unknown, &IClassFactory::IID, &mut other) },
            CLASS_E_CLASSNOTAVAILABLE
        );
        assert!(other.is_null());
    }
}

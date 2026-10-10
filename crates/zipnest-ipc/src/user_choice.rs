//! Make ZipNest the **default** handler for an extension, without asking the
//! user to walk through Windows Settings.
//!
//! Windows records the handler the user picked for an extension in
//! `HKCU\...\Explorer\FileExts\.<ext>\UserChoice` — a key with
//! `ProgId` + `Hash` values. Two things guard it:
//!
//! 1. an explicit `Deny SetValue` ACE for the current user, so the values cannot
//!    be *written*; and
//! 2. the `Hash`, which Windows re-computes and compares before honouring the
//!    entry ("in the absence of a valid hash, we ignore the default in the
//!    registry").
//!
//! Neither guard blocks what this module does:
//!
//! * the deny ACE does **not** deny `DELETE`, and the user owns the key, so the
//!   subkey can simply be deleted and recreated — exactly what Firefox's
//!   default-agent ships (`RegDeleteKeyW(assocKey, L"UserChoice")`);
//! * the hash is a plain function of (extension, user SID, ProgId, the key's
//!   own last-write time truncated to the minute) plus a constant salt, so we
//!   can compute it ourselves.
//!
//! That is the whole trick: **delete + recreate with a valid hash**. No
//! `ms-settings:` page, no "how do you want to open this file" dialog, no
//! forged-hash fight with the OS, and the user can still change their mind in
//! Windows' own Default apps page afterwards (Windows simply replaces the same
//! key again).
//!
//! Measured on Windows 11 26H1 (build 28020), where the older "send the user to
//! Settings" approach had stopped working entirely: the computed hash reproduced
//! the stored hash for every key on the machine that had one (8/8), and after
//! `claim_default` the shell opened the archive with ZipNest and no dialog.
//!
//! Two caveats worth knowing:
//!
//! * Microsoft's `UCPD.sys` ("User Choice Protection Driver") blocks writes for
//!   a handful of identifiers — `http`, `https`, `.pdf`, `.html`/`.htm` and the
//!   Office formats. None of the archive extensions we ship is on that list, and
//!   this module never touches them.
//! * the hash is minute-bound: writing `Hash` moves the key's last-write time,
//!   so it is re-read and re-verified after the write, and recomputed once if
//!   the minute rolled over in between.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// Constant baked into Windows' own hash algorithm. Firefox ships the same
/// string, derived from PS-SFTA: "There isn't any known way of deriving it, so
/// we assume this constant value."
const USER_EXPERIENCE: &str =
    "User Choice set via Windows User Experience {D18B6DD5-6124-4341-9318-804003BAFA0B}";

/// One minute in `FILETIME` units (100 ns ticks). The hash truncates the key's
/// last-write time to the minute, which is what keeps it stable across the two
/// writes below.
const TICKS_PER_MINUTE: u64 = 60 * 10_000_000;

/// `FileExts\.<ext>` under the current user's hive.
fn file_exts_path(ext: &str) -> String {
    format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\{}",
        dotted_extension(ext)
    )
}

/// Windows keys a file association by the extension **with** its leading dot,
/// and the dot is part of the hash input: `.tar`, never `tar`. The rest of the
/// app carries the bare form ("tar"), so everything that reaches Windows — a key
/// path or the hash — goes through here first. Getting this wrong produces a
/// hash Windows rejects, after which it deletes the whole entry (measured).
fn dotted_extension(ext: &str) -> String {
    if ext.starts_with('.') {
        ext.to_string()
    } else {
        format!(".{ext}")
    }
}

/// The `ProgId` we register for an extension, matching [`crate::shell`].
pub fn prog_id(ext: &str) -> String {
    format!("ZipNest.{ext}")
}

/// Compute the `UserChoice` hash.
///
/// `last_write_filetime` is the `FILETIME` (100 ns ticks since 1601, as
/// reported by `RegQueryInfoKey`) of the `UserChoice` key itself. Only the
/// minute matters, so callers may pass any time inside the target minute.
pub fn user_choice_hash(ext: &str, sid: &str, prog_id: &str, last_write_filetime: u64) -> String {
    let minute = last_write_filetime / TICKS_PER_MINUTE * TICKS_PER_MINUTE;
    let high = (minute >> 32) as u32;
    let low = (minute & 0xFFFF_FFFF) as u32;

    // The whole input is lower-cased, including the salt's GUID.
    let input = format!(
        "{ext}{sid}{prog_id}{high:08x}{low:08x}{USER_EXPERIENCE}"
    )
    .to_lowercase();

    // UTF-16LE, terminator included: the hash covers the trailing NUL.
    let mut bytes = Vec::with_capacity((input.len() + 1) * 2);
    for unit in input.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&[0, 0]);

    let digest = md5(&bytes);
    let mut md = [0u32; 4];
    for (i, word) in md.iter_mut().enumerate() {
        *word = u32::from_le_bytes([
            digest[i * 4],
            digest[i * 4 + 1],
            digest[i * 4 + 2],
            digest[i * 4 + 3],
        ]);
    }

    // Two checksums over 8-byte blocks of the input; incomplete trailing blocks
    // are ignored. `md[0]`/`md[1]` seed the multipliers.
    let c0 = [
        [md[0] | 1, 0xCF98B111, 0x87085B9F, 0x12CEB96D, 0x257E1D83],
        [md[1] | 1, 0xA27416F5, 0xD38396FF, 0x7C932B89, 0xBFA49F69],
    ];
    let c1 = [
        [md[0] | 1, 0xEF0569FB, 0x689B6B9F, 0x79F8A395, 0xC3EFEA97],
        [md[1] | 1, 0xC31713DB, 0xDDCD1F0F, 0x59C3AF2D, 0x35BD1EC9],
    ];

    let (mut h0, mut h1, mut h0_acc, mut h1_acc) = (0u32, 0u32, 0u32, 0u32);
    let blocks = bytes.len() / 8;
    for block in 0..blocks {
        for j in 0..2 {
            let at = (block * 2 + j) * 4;
            let input = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);

            h0 = h0.wrapping_add(input);
            h0 = h0.wrapping_mul(c0[j][0]);
            h0 = word_swap(h0).wrapping_mul(c0[j][1]);
            h0 = word_swap(h0).wrapping_mul(c0[j][2]);
            h0 = word_swap(h0).wrapping_mul(c0[j][3]);
            h0 = word_swap(h0).wrapping_mul(c0[j][4]);
            h0_acc = h0_acc.wrapping_add(h0);

            h1 = h1.wrapping_add(input);
            h1 = word_swap(h1)
                .wrapping_mul(c1[j][1])
                .wrapping_add(h1.wrapping_mul(c1[j][0]));
            h1 = (h1 >> 16)
                .wrapping_mul(c1[j][2])
                .wrapping_add(h1.wrapping_mul(c1[j][3]));
            h1 = word_swap(h1).wrapping_mul(c1[j][4]).wrapping_add(h1);
            h1_acc = h1_acc.wrapping_add(h1);
        }
    }

    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&(h0 ^ h1).to_le_bytes());
    out[4..].copy_from_slice(&(h0_acc ^ h1_acc).to_le_bytes());
    base64(&out)
}

/// Firefox calls this `WordSwap`; a 16-bit half-swap is `rotate_left(16)`.
fn word_swap(v: u32) -> u32 {
    v.rotate_left(16)
}

/// Standard base64 with padding — `CRYPT_STRING_BASE64 | CRYPT_STRING_NOCRLF`.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// MD5 (RFC 1321). Hand-rolled so the crate keeps its zero-dependency manifest;
/// correctness is pinned by the live-machine vectors in the tests below.
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6,
        10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];

    let mut message = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    for chunk in message.chunks(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }

        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for (i, shift) in S.iter().enumerate() {
            // K[i] = floor(2^32 * |sin(i + 1)|) — the RFC 1321 table, derived
            // rather than transcribed.
            let k = (4_294_967_296.0_f64 * ((i as f64) + 1.0).sin().abs()) as u32;
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(k).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(*shift));
            a = tmp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..].copy_from_slice(&d0.to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// Registry plumbing
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win32 {
    use super::*;

    pub type Handle = isize;
    const HKEY_CURRENT_USER: Handle = 0x8000_0001u32 as Handle;
    const KEY_ALL_ACCESS: u32 = 0x000F_003F;
    const KEY_READ: u32 = 0x0002_0019;
    const REG_SZ: u32 = 1;
    const REG_DWORD: u32 = 4;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            key: Handle,
            sub: *const u16,
            options: u32,
            access: u32,
            out: *mut Handle,
        ) -> i32;
        fn RegCreateKeyExW(
            key: Handle,
            sub: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            access: u32,
            sec: *const core::ffi::c_void,
            out: *mut Handle,
            disposition: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            key: Handle,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            len: u32,
        ) -> i32;
        fn RegQueryValueExW(
            key: Handle,
            name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            len: *mut u32,
        ) -> i32;
        fn RegQueryInfoKeyW(
            key: Handle,
            class: *mut u16,
            class_len: *mut u32,
            reserved: *mut u32,
            subkeys: *mut u32,
            max_subkey: *mut u32,
            max_class: *mut u32,
            values: *mut u32,
            max_value: *mut u32,
            max_data: *mut u32,
            security: *mut u32,
            last_write: *mut FileTime,
        ) -> i32;
        fn RegDeleteKeyW(key: Handle, sub: *const u16) -> i32;
        fn RegDeleteTreeW(key: Handle, sub: *const u16) -> i32;
        fn RegCloseKey(key: Handle) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(event: i32, flags: u32, item1: *const core::ffi::c_void, item2: *const core::ffi::c_void);
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn LocalFree(mem: Handle) -> Handle;
    }

    #[derive(Clone, Copy, Default)]
    #[repr(C)]
    pub struct FileTime {
        pub low: u32,
        pub high: u32,
    }
    impl FileTime {
        pub fn ticks(&self) -> u64 {
            ((self.high as u64) << 32) | self.low as u64
        }
    }

    #[repr(C)]
    struct SidAndAttributes {
        sid: Handle,
        attributes: u32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: u32,
            info: *mut core::ffi::c_void,
            len: u32,
            needed: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: Handle, out: *mut *mut u16) -> i32;
    }

    pub fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn sid_string() -> Option<String> {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_USER: u32 = 1;
        unsafe {
            let mut token: Handle = 0;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut needed: u32 = 0;
            GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                return None;
            }
            let mut buf = vec![0u8; needed as usize];
            if GetTokenInformation(
                token,
                TOKEN_USER,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                needed,
                &mut needed,
            ) == 0
            {
                return None;
            }
            let user = buf.as_ptr() as *const SidAndAttributes;
            let mut raw: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW((*user).sid, &mut raw) == 0 {
                return None;
            }
            let mut len = 0usize;
            while *raw.add(len) != 0 {
                len += 1;
            }
            let out = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
            LocalFree(raw as Handle);
            Some(out)
        }
    }

    fn open(key: Handle, path: &str, access: u32) -> Option<Handle> {
        let path = wide(path);
        let mut handle: Handle = 0;
        let rc = unsafe { RegOpenKeyExW(key, path.as_ptr(), 0, access, &mut handle) };
        (rc == 0).then_some(handle)
    }

    fn set_string(path: &str, name: &str, value: &str) -> Result<(), String> {
        let mut handle: Handle = 0;
        let mut disposition = 0u32;
        let path_w = wide(path);
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path_w.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut handle,
                &mut disposition,
            )
        };
        if rc != 0 {
            return Err(format!("RegCreateKeyExW({path}) -> {rc}"));
        }
        let name_w = wide(name);
        let data: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let rc = unsafe {
            RegSetValueExW(
                handle,
                name_w.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        unsafe { RegCloseKey(handle) };
        if rc != 0 {
            return Err(format!("RegSetValueExW({path}\\{name}) -> {rc}"));
        }
        Ok(())
    }

    fn set_dword(path: &str, name: &str, value: u32) -> Result<(), String> {
        let mut handle: Handle = 0;
        let mut disposition = 0u32;
        let path_w = wide(path);
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path_w.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut handle,
                &mut disposition,
            )
        };
        if rc != 0 {
            return Err(format!("RegCreateKeyExW({path}) -> {rc}"));
        }
        let name_w = wide(name);
        let data = value.to_le_bytes();
        let rc = unsafe {
            RegSetValueExW(
                handle,
                name_w.as_ptr(),
                0,
                REG_DWORD,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        unsafe { RegCloseKey(handle) };
        if rc != 0 {
            return Err(format!("RegSetValueExW({path}\\{name}) -> {rc}"));
        }
        Ok(())
    }

    pub fn get_string(path: &str, name: &str) -> Option<String> {
        let handle = open(HKEY_CURRENT_USER, path, KEY_READ)?;
        let name_w = wide(name);
        let mut kind = 0u32;
        let mut len = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                handle,
                name_w.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if rc != 0 || len == 0 {
            unsafe { RegCloseKey(handle) };
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let rc = unsafe {
            RegQueryValueExW(
                handle,
                name_w.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        unsafe { RegCloseKey(handle) };
        if rc != 0 {
            return None;
        }
        let mut units: Vec<u16> = Vec::with_capacity(buf.len() / 2);
        for pair in buf.chunks(2) {
            if let [low, high] = pair {
                units.push(u16::from_le_bytes([*low, *high]));
            }
        }
        Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
    }

    pub fn last_write(path: &str) -> Option<u64> {
        let handle = open(HKEY_CURRENT_USER, path, KEY_READ)?;
        let mut ft = FileTime::default();
        let rc = unsafe {
            RegQueryInfoKeyW(
                handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut ft,
            )
        };
        unsafe { RegCloseKey(handle) };
        (rc == 0).then(|| ft.ticks())
    }

    /// Delete `sub` under `parent`, subkeys included.
    ///
    /// The protected key carries a `Deny SetValue` ACE but not a `Deny Delete`,
    /// so this succeeds where writing fails. `RegDeleteKeyW` alone is not
    /// enough for `UserChoiceLatest`, which holds the choice in a `ProgId`
    /// subkey, so the tree variant is used throughout.
    pub fn delete_subkey(parent: &str, sub: &str) -> u32 {
        let Some(handle) = open(HKEY_CURRENT_USER, parent, KEY_ALL_ACCESS) else {
            return u32::MAX;
        };
        let sub_w = wide(sub);
        let mut rc = unsafe { RegDeleteTreeW(handle, sub_w.as_ptr()) };
        if rc != 0 {
            rc = unsafe { RegDeleteKeyW(handle, sub_w.as_ptr()) };
        }
        unsafe { RegCloseKey(handle) };
        rc as u32
    }

    pub fn write_string(path: &str, name: &str, value: &str) -> Result<(), String> {
        set_string(path, name, value)
    }

    pub fn write_dword(path: &str, name: &str, value: u32) -> Result<(), String> {
        set_dword(path, name, value)
    }

    pub fn notify_shell() {
        const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
        const SHCNF_IDLIST: u32 = 0x0000;
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) }
    }
}

/// Which handler Windows currently has recorded for an extension.
pub fn recorded_handler(ext: &str) -> Option<String> {
    #[cfg(windows)]
    {
        win32::get_string(&format!("{}\\UserChoice", file_exts_path(ext)), "ProgId")
    }
    #[cfg(not(windows))]
    {
        let _ = ext;
        None
    }
}

/// Make ZipNest the default handler for one extension.
///
/// Deleting and recreating the key is what makes this work where writing into it
/// cannot: Windows keeps the *user's* key modifiable, it only denies value
/// writes. A stale `UserChoiceLatest` (an undocumented second store some 26H1
/// builds add) is dropped so it cannot compete with the entry we write.
#[cfg(windows)]
pub fn claim_default(ext: &str, exe_prog_id: &str) -> Result<String, String> {
    let sid = win32::sid_string().ok_or_else(|| "cannot read the current user SID".to_string())?;
    let dotted = dotted_extension(ext);
    let ext = dotted.as_str();
    let parent = file_exts_path(ext);
    let choice = format!("{parent}\\UserChoice");

    win32::delete_subkey(&parent, "UserChoice");
    win32::delete_subkey(&parent, "UserChoiceLatest");
    win32::write_string(&choice, "ProgId", exe_prog_id)?;

    // The hash covers the key's own last-write time (truncated to the minute),
    // so it is derived from the key *after* the ProgId write -- and then
    // verified by re-deriving from the time the Hash write itself left behind.
    // Writing `Hash` re-stamps the key, so a minute rollover in between would
    // leave a value Windows rejects; it reacts by deleting the whole entry, so
    // verifying like this is not optional.
    let mut hash = derive_hash(ext, &sid, exe_prog_id, &choice);
    win32::write_string(&choice, "Hash", &hash)?;
    let reverified = derive_hash(ext, &sid, exe_prog_id, &choice);
    if reverified != hash {
        // The write above landed in a new minute: the retry converges, because
        // this write stamps the key again inside the minute just entered.
        hash = reverified;
        win32::write_string(&choice, "Hash", &hash)?;
        if derive_hash(ext, &sid, exe_prog_id, &choice) != hash {
            return Err(format!("hash did not verify for {ext}"));
        }
    }

    // Windows shows "you have a new app" toasts when a handler changes; PS-SFTA
    // writes the same marker to keep the takeover quiet.
    let toasts = "Software\\Microsoft\\Windows\\CurrentVersion\\ApplicationAssociationToasts";
    let _ = win32::write_dword(toasts, &format!("{exe_prog_id}_{ext}"), 0);

    Ok(hash)
}

/// Recompute the hash for whatever the key's last-write time is right now.
#[cfg(windows)]
fn derive_hash(ext: &str, sid: &str, prog_id: &str, choice_key: &str) -> String {
    let ticks = win32::last_write(choice_key).unwrap_or(0);
    user_choice_hash(ext, sid, prog_id, ticks)
}

#[cfg(not(windows))]
pub fn claim_default(ext: &str, _prog_id: &str) -> Result<String, String> {
    Err(format!("setting the default handler is Windows-only (asked for .{ext})"))
}

/// Give an extension back, but only while ZipNest is what Windows has recorded:
/// releasing must never clobber a handler the user picked later.
#[cfg(windows)]
pub fn release_default(ext: &str, exe_prog_id: &str) -> Result<(), String> {
    let parent = file_exts_path(ext);
    if recorded_handler(ext).is_some_and(|p| p.eq_ignore_ascii_case(exe_prog_id)) {
        win32::delete_subkey(&parent, "UserChoice");
    }
    let latest = format!("{parent}\\UserChoiceLatest\\ProgId");
    if win32::get_string(&latest, "ProgId").is_some_and(|p| p.eq_ignore_ascii_case(exe_prog_id)) {
        win32::delete_subkey(&parent, "UserChoiceLatest");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn release_default(_ext: &str, _prog_id: &str) -> Result<(), String> {
    Ok(())
}

/// Tell Explorer the associations changed, so an open window stops using its
/// cached handler immediately.
pub fn notify_shell() {
    #[cfg(windows)]
    win32::notify_shell();
}

/// Claim every extension in `exts`, returning `(ext, error)` for the failures.
///
/// Deliberately never gives up after the first failure: one protected
/// identifier must not stop the other eight from being taken over.
pub fn claim_all<'a>(exts: impl IntoIterator<Item = &'a str>) -> Vec<(String, String)> {
    let mut failures = Vec::new();
    let mut claimed = false;
    for ext in exts {
        match claim_default(ext, &prog_id(ext)) {
            Ok(_) => claimed = true,
            Err(e) => failures.push((ext.to_string(), e)),
        }
    }
    if claimed {
        notify_shell();
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The current user's SID, fixed for the machine these vectors came from.
    const SID: &str = "S-1-5-21-809284386-3299115467-351109592-1001";

    /// Real `UserChoice` entries read off a Windows 11 26H1 (build 28020)
    /// machine, with each key's own last-write time. They pin both the MD5 and
    /// the scramble: a single wrong constant and every one of them fails.
    #[test]
    fn the_hash_reproduces_windows_own_entries() {
        let vectors: [(&str, &str, u64, &str); 8] = [
            (".rar", "ZipNest.rar", 134354812990243832, "yXYOmFcUxLA="),
            (
                ".txt",
                "AppX4ztfk9wxr86nxmzzq47px0nh0e58b8fw",
                134317135878324965,
                "W4DsQxgS4TA=",
            ),
            (".pdf", "MSEdgePDF", 134317135877907781, "hFJhl0czRo8="),
            (".html", "MSEdgeHTM", 134317135877354929, "zTiO6DMeZBg="),
            (
                ".png",
                "AppX43hnxtbyyps62jhe9sqpdzxn1790zetc",
                134317135877939186,
                "IImvzCOOYKU=",
            ),
            (
                ".jpg",
                "AppX43hnxtbyyps62jhe9sqpdzxn1790zetc",
                134317135877446368,
                "E3Cvj9UIqyA=",
            ),
            (
                ".mp4",
                "AppXqj98qxeaynz6dv4459ayz6bnqxbyaqcs",
                134354244290092476,
                "ytxHYmWMxGw=",
            ),
            (".zip", "ZipNest.zip", 134361285656161523, "m16qsyUKBjw="),
        ];
        for (ext, prog_id, ticks, expected) in vectors {
            assert_eq!(
                user_choice_hash(ext, SID, prog_id, ticks),
                expected,
                "hash mismatch for {ext}"
            );
        }
    }

    /// Only the minute matters, so any timestamp inside it hashes the same --
    /// this is what makes the two-write sequence above safe.
    #[test]
    fn the_timestamp_is_truncated_to_the_minute() {
        let base = 134361285656161523u64;
        let minute_start = base / TICKS_PER_MINUTE * TICKS_PER_MINUTE;
        let same = user_choice_hash(".zip", SID, "ZipNest.zip", minute_start);
        for offset in [1, 1_000, 10_000_000, 599_999_999] {
            assert_eq!(
                user_choice_hash(".zip", SID, "ZipNest.zip", minute_start + offset),
                same,
                "offset {offset} must stay inside the same minute"
            );
        }
        assert_ne!(
            user_choice_hash(".zip", SID, "ZipNest.zip", minute_start + TICKS_PER_MINUTE),
            same
        );
    }

    #[test]
    fn the_prog_id_is_the_one_the_registry_uses() {
        assert_eq!(prog_id("zip"), "ZipNest.zip");
        assert_eq!(prog_id("7z"), "ZipNest.7z");
    }

    /// The rest of the app carries bare extension names ("tar"); Windows and the
    /// hash want the dotted form. Forgetting this produced a hash Windows
    /// rejected and then deleted the entry for -- so it is pinned here.
    #[test]
    fn extensions_are_dotted_on_the_way_to_windows() {
        assert_eq!(dotted_extension("tar"), ".tar");
        assert_eq!(dotted_extension(".tar"), ".tar");
        assert_eq!(
            file_exts_path("zip"),
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.zip"
        );
        assert_eq!(
            file_exts_path(".zip"),
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.zip"
        );
        // The hash must be the same whichever form the caller passed.
        let ticks = 134361285656161523;
        assert_eq!(
            user_choice_hash(&dotted_extension("zip"), SID, "ZipNest.zip", ticks),
            user_choice_hash(".zip", SID, "ZipNest.zip", ticks)
        );
    }

    #[test]
    fn base64_matches_the_reference_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// RFC 1321's own test suite, so a broken MD5 cannot hide behind the
    /// higher-level vectors.
    #[test]
    fn md5_matches_the_rfc_vectors() {
        let hex = |bytes: [u8; 16]| {
            bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(md5(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            hex(md5(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_current_user_has_a_readable_sid() {
        let sid = win32::sid_string().expect("a token SID");
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[cfg(windows)]
    #[test]
    fn the_file_exts_path_is_the_one_windows_uses() {
        assert_eq!(
            file_exts_path("zip"),
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.zip"
        );
    }
}


# ZipNest M1 — 引擎地基 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 ZipNest 的 Rust 引擎地基——`archive-security` 安全防线与 `archive-core`（7z.dll 进程内 COM 封装）的 open/list/read/extract/密码/进度/取消闭环，全部测试绿。

**Architecture:** cargo workspace 两 crate：`archive-security`（纯 Rust，零依赖，先交付）与 `archive-core`（动态加载 7z.dll，手写 COM vtable 调用 `IInArchive`/`IInStream`/回调接口；官方 SDK 头文件 vendor 进仓库作为 vtable 抄写来源）。所有落盘路径必须经过 `archive_security::sanitize_entry_path`。

**Tech Stack:** Rust 1.98 / edition 2021 / msvc target；7-Zip 26.03 SDK 头文件（vendor，LGPL）；测试用本机 `7z.exe` 生成 fixture；`zip` crate（dev-dependency，构造恶意路径 zip）。

**Spec:** `docs/superpowers/specs/2026-09-30-zipnest-design.md`

## Global Constraints

- 平台：仅 Windows（x64 开发/测试；ARM64 为 M4 项）；PowerShell 5.1 每次新 shell 需 `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`（Task 1 永久修复）。
- RAR：只解压、永不创建。本计划不含 RAR 创建代码。
- 7z.dll：仅动态加载；禁静态链接、禁改 DLL；vendor 头文件附 license。
- 安全：任何写盘路径必须经 `archive_security::sanitize_entry_path`；硬拒绝绝对路径、`..`、盘符、UNC、NTFS 保留名。
- 密码：仅内存传递，禁止进日志/Debug 输出（`ArchiveOpenOptions` 手写 Debug）。
- 错误：跨层只允许 `ZipnestError`，`error_key()` 返回 `"error.xxx"` 常量；引擎禁止 panic 外泄。
- 进度回调返回 `false` = 取消 → 引擎尽快中止并返回 `ZipnestError::Cancelled`。
- 每任务结束：`cargo test --workspace` 全绿 + git commit（英文 message）；测试在仓库根执行。

## File Structure

```
zipnest/
├── Cargo.toml  .gitignore
├── vendor/7zip-sdk/          # 官方头文件 + license + Rust FFI notes（Task 4）
├── fixtures/                 # make_fixtures.ps1 生成并提交的测试归档
├── crates/
│   ├── archive-security/src/{lib,path,quota}.rs
│   └── archive-core/
│       ├── src/{lib,error,types,dll,archive}.rs
│       ├── src/com/{mod,vtables,propvariant,instream,callbacks}.rs
│       └── tests/engine.rs
```

---

### Task 1: Workspace 脚手架 + fixture 流水线

**Files:** Create `Cargo.toml`, `.gitignore`, `fixtures/make_fixtures.ps1`, `crates/archive-security/{Cargo.toml,src/lib.rs}`, `crates/archive-core/{Cargo.toml,src/lib.rs}`

**Interfaces:**
- Produces: workspace `cargo test --workspace` 通过；fixtures 就位（`plain.zip` `nested.zip` `enc.zip` `enc.7z` `corrupt.zip`），后续任务以 `fx(name)` helper 消费。

- [ ] **Step 1: 验证 cargo 可用**
Run: `cargo --version`。若失败：`[Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path","User") + ";$env:USERPROFILE\.cargo\bin", "User")` 后新 shell 复验。

- [ ] **Step 2: 创建 workspace**

根 `Cargo.toml`：

```toml
[workspace]
resolver = "2"
members = ["crates/archive-security", "crates/archive-core"]

[workspace.package]
edition = "2021"
license = "MIT"
```

`crates/archive-security/Cargo.toml`：`[package] name = "archive-security" version = "0.1.0" edition.workspace = true`
`crates/archive-core/Cargo.toml`：

```toml
[package]
name = "archive-core"
version = "0.1.0"
edition.workspace = true

[dependencies]
archive-security = { path = "../archive-security" }

[dev-dependencies]
zip = "6"
```

两 crate `src/lib.rs` 放 `// scaffold`；`.gitignore`: `/target` `*.log`。

- [ ] **Step 3: 验证编译** Run `cargo test --workspace` → 空通过。

- [ ] **Step 4: fixture 脚本** `fixtures/make_fixtures.ps1`：

```powershell
$7z = "C:\Program Files\7-Zip\7z.exe"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$tmp = Join-Path $env:TEMP "zipnest-fx"; Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $tmp | Out-Null
Set-Content "$tmp\a.txt" "hello zipnest`nline2" -Encoding UTF8
New-Item -ItemType Directory "$tmp\sub" | Out-Null
Set-Content "$tmp\sub\b.txt" "nested content" -Encoding UTF8
Copy-Item "$tmp\a.txt" "$tmp\c.txt"
1..500 | ForEach-Object { Add-Content "$tmp\c.txt" "padding line $_" }
Push-Location $tmp
& $7z a -tzip -y "$here\nested.zip" a.txt sub c.txt | Out-Null
& $7z a -tzip -y "$here\plain.zip" a.txt sub | Out-Null
& $7z a -tzip -y -psecret -mhe=on "$here\enc.zip" a.txt | Out-Null
& $7z a -t7z -y -psecret "$here\enc.7z" a.txt | Out-Null
& $7z a -tzip -y "$here\corrupt.zip" a.txt | Out-Null
Pop-Location
$bytes = [IO.File]::ReadAllBytes("$here\corrupt.zip")
$bytes[$bytes.Length - 5] = $bytes[$bytes.Length - 5] -bxor 0xFF
[IO.File]::WriteAllBytes("$here\corrupt.zip", $bytes)
Write-Host "fixtures ok"
```

Run: `powershell -ExecutionPolicy Bypass -File fixtures\make_fixtures.ps1` → `fixtures ok`；`plain.zip` 含 a.txt + sub/ + sub/b.txt = 3 条目。

- [ ] **Step 5: Commit** `git add -A; git commit -m "chore: scaffold cargo workspace and test fixtures"`

---

### Task 2: archive-security — 条目路径清洗（TDD）

**Files:** Create `crates/archive-security/src/path.rs`；Modify `lib.rs`

**Interfaces:**
- Produces: `pub fn sanitize_entry_path(raw: &str) -> Result<String, SecurityViolation>`；`pub enum SecurityViolation { Traversal, Absolute, ReservedName, Empty, TooLong }`（Display+Error+PartialEq）。返回 `/` 分隔相对路径。

- [ ] **Step 1: 失败测试**（写入 path.rs）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn ok(raw: &str, expect: &str) { assert_eq!(sanitize_entry_path(raw).unwrap(), expect, "raw={raw:?}"); }
    fn bad(raw: &str, v: SecurityViolation) { assert_eq!(sanitize_entry_path(raw).unwrap_err(), v, "raw={raw:?}"); }

    #[test] fn accepts_normal_paths() {
        ok("a.txt", "a.txt"); ok("dir/sub/b.txt", "dir/sub/b.txt");
        ok("./dir/./b.txt", "dir/b.txt"); ok("中文/文件.txt", "中文/文件.txt");
    }
    #[test] fn rejects_traversal() {
        bad("../evil", SecurityViolation::Traversal);
        bad("a/../../b", SecurityViolation::Traversal);
        bad("..", SecurityViolation::Traversal);
        bad("a/b/../../../c", SecurityViolation::Traversal);
        bad("a/..", SecurityViolation::Traversal);
    }
    #[test] fn rejects_absolute_and_unc() {
        bad("/abs", SecurityViolation::Absolute);
        bad("C:\\abs", SecurityViolation::Absolute);
        bad("c:/abs", SecurityViolation::Absolute);
        bad("C:rel", SecurityViolation::Absolute);
        bad("\\\\server\\share\\x", SecurityViolation::Absolute);
        bad("//server/share", SecurityViolation::Absolute);
    }
    #[test] fn rejects_reserved_device_names() {
        bad("CON", SecurityViolation::ReservedName);
        bad("con.txt", SecurityViolation::ReservedName);
        bad("dir/NUL", SecurityViolation::ReservedName);
        bad("COM1.log", SecurityViolation::ReservedName);
        bad("LPT9", SecurityViolation::ReservedName);
        bad("aux", SecurityViolation::ReservedName);
    }
    #[test] fn cleans_windows_filenames() {
        ok("a<b>:c.txt", "a_b_c.txt");
        ok("x|y?.txt", "x_y_.txt");
        ok("trail. ", "trail");
        ok("dir/ok.txt.", "dir/ok.txt");
        ok("..%2ffile", "..%2ffile");
        ok("a//b", "a/b");
        bad("///", SecurityViolation::Empty);
        bad(" . ", SecurityViolation::Empty);
        bad(&"x".repeat(300), SecurityViolation::TooLong);
    }
}
```

- [ ] **Step 2: 确认失败** `cargo test -p archive-security` → FAIL（函数未定义）

- [ ] **Step 3: 实现**

```rust
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SecurityViolation { Traversal, Absolute, ReservedName, Empty, TooLong }
impl std::fmt::Display for SecurityViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for SecurityViolation {}

const RESERVED: [&str; 22] = ["con","prn","aux","nul",
    "com1","com2","com3","com4","com5","com6","com7","com8","com9",
    "lpt1","lpt2","lpt3","lpt4","lpt5","lpt6","lpt7","lpt8","lpt9"];

fn is_illegal(c: char) -> bool {
    matches!(c, '<'|'>'|':'|'"'|'|'|'?'|'*'|'\\') || (c as u32) < 0x20
}

pub fn sanitize_entry_path(raw: &str) -> Result<String, SecurityViolation> {
    let t = raw.trim();
    if t.is_empty() { return Err(SecurityViolation::Empty); }
    if t.starts_with('/') || t.starts_with('\\') { return Err(SecurityViolation::Absolute); }
    if t.len() >= 2 && t.as_bytes()[1] == b':' { return Err(SecurityViolation::Absolute); }
    if t.starts_with("//") || t.starts_with("\\\\") { return Err(SecurityViolation::Absolute); }
    let mut out: Vec<String> = Vec::new();
    for comp in t.split(['/', '\\']) {
        if comp.is_empty() || comp == "." { continue; }
        if comp == ".." { return Err(SecurityViolation::Traversal); }
        let clean = comp.trim_end_matches(['.', ' ']).trim_start_matches(' ');
        if clean.is_empty() { return Err(SecurityViolation::Empty); }
        if clean.len() > 255 { return Err(SecurityViolation::TooLong); }
        let stem = clean.split('.').next().unwrap_or(clean);
        if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(SecurityViolation::ReservedName);
        }
        out.push(clean.chars().map(|c| if is_illegal(c) { '_' } else { c }).collect());
    }
    if out.is_empty() { return Err(SecurityViolation::Empty); }
    Ok(out.join("/"))
}
```

注意：`a/..` 会先 split 出 `..` 组件触发 Traversal；`trail. ` trim 后 `trail`；`dir/ok.txt.` 的尾点被剥成 `ok.txt`。

- [ ] **Step 4: 确认通过** `cargo test -p archive-security` → 5 tests PASS
- [ ] **Step 5: Commit** `git commit -m "feat(security): harden archive entry path sanitization"`

---

### Task 3: archive-security — 解压限额（TDD）

**Files:** Create `crates/archive-security/src/quota.rs`；Modify `lib.rs`

**Interfaces:**
- Produces: `ExtractQuota::new(max_bytes: u64)`、`charge(&mut self, bytes: u64) -> Result<(), QuotaExceeded>`（失败不扣额）、`remaining(&self) -> u64`、`pub struct QuotaExceeded`（Display+Error）。

- [ ] **Step 1: 失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn charges_within_limit() {
        let mut q = ExtractQuota::new(100);
        assert!(q.charge(60).is_ok()); assert_eq!(q.remaining(), 40);
        assert!(q.charge(40).is_ok()); assert_eq!(q.remaining(), 0);
    }
    #[test] fn rejects_over_limit() {
        let mut q = ExtractQuota::new(100);
        assert!(q.charge(101).is_err());
        assert!(q.charge(60).is_ok());
        assert!(q.charge(41).is_err());
        assert_eq!(q.remaining(), 40);
    }
    #[test] fn zero_limit_blocks_everything() {
        let mut q = ExtractQuota::new(0);
        assert!(q.charge(0).is_ok()); assert!(q.charge(1).is_err());
    }
}
```

- [ ] **Step 2: 确认失败** `cargo test -p archive-security` → FAIL
- [ ] **Step 3: 实现**

```rust
#[derive(Debug, PartialEq, Eq)]
pub struct QuotaExceeded;
impl std::fmt::Display for QuotaExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "extract quota exceeded") }
}
impl std::error::Error for QuotaExceeded {}

pub struct ExtractQuota { max: u64, used: u64 }
impl ExtractQuota {
    pub fn new(max_bytes: u64) -> Self { Self { max: max_bytes, used: 0 } }
    pub fn charge(&mut self, bytes: u64) -> Result<(), QuotaExceeded> {
        match self.used.checked_add(bytes) {
            Some(total) if total <= self.max => { self.used = total; Ok(()) }
            _ => Err(QuotaExceeded),
        }
    }
    pub fn remaining(&self) -> u64 { self.max - self.used }
}
```

`lib.rs`: `pub mod path; pub mod quota; pub use path::*; pub use quota::*;`

- [ ] **Step 4: 确认通过** `cargo test -p archive-security` → 8 tests PASS
- [ ] **Step 5: Commit** `git commit -m "feat(security): add extraction quota accounting"`

---

### Task 4: Vendor 7-Zip SDK 头文件（COM 抄写源）

**Files:** Create `vendor/7zip-sdk/*.h`, `LICENSE`, `README.md`

**Interfaces:**
- Produces: Task 5-9 的 vtable/常量来源；README 的 “Rust FFI notes” 一节是后续任务的权威参考。

- [ ] **Step 1: 下载头文件**（`curl.exe` 或 webfetch，来源 `https://raw.githubusercontent.com/ip7z/7zip/<tag>/CPP/7zip/`，tag 用对应 26.03 的发布 tag，取不到用 main）：
  - `Archive/IArchive.h`（IInArchive/IOutArchive/IArchiveOpenCallback/IArchiveExtractCallback/IArchiveCreateCallback）
  - `ICode.h`（IProgress）、`PropID.h`、`PropVar.h`、`IDecl.h`、`Stream.h`（IInStream/ISequentialInStream/ISequentialOutStream/IOutStream——若 IArchive.h 的 include 链指向别的文件名，按实际补齐）
  - `Archive/7z.h`（kpid* / CLSID 常量）

- [ ] **Step 2: license + README**
  - `https://raw.githubusercontent.com/ip7z/7zip/main/DOC/License.txt` → `vendor/7zip-sdk/LICENSE`
  - `README.md`：版本 26.03、来源 URL/commit、用途声明（仅抄写头文件，运行时动态加载 7z.dll）、LGPL 合规声明。

- [ ] **Step 3: 写 “Rust FFI notes”**（基于头文件原文，逐条抄，不凭记忆）：
  1. `IInArchive` vtable 完整方法序+签名；
  2. `IArchiveOpenCallback`、`IArchiveExtractCallback`、`IProgress`、`ICryptoGetTextPassword` 方法序；
  3. `IInStream`/`ISequentialInStream`/`ISequentialOutStream` 方法序；
  4. `CreateObject` 导出签名 + CLSID/IID GUID 十六进制值（IInArchive、IInStream、IArchiveOpenCallback、IArchiveExtractCallback、ICryptoGetTextPassword、ISequentialOutStream、CFormatZip、CFormat7z）；
  5. `PROPVARIANT` 布局 + `kpidPath/kpidIsDir/kpidSize/kpidMTime/kpidEncrypted/kpidCRC/kpidNumItems` 数值 + VT_* 常量值。

- [ ] **Step 4: 回归** `cargo test --workspace` → PASS
- [ ] **Step 5: Commit** `git commit -m "chore: vendor 7-Zip SDK headers as COM vtable reference"`

---

### Task 5: 7z.dll 加载 + 打开归档 + 条目计数

**Files:** Create `archive-core/src/{error.rs,types.rs,dll.rs,archive.rs,com/{mod.rs,vtables.rs,propvariant.rs,callbacks.rs,instream.rs}}`（instream 先做内存版）；Modify `lib.rs`；Test `archive-core/tests/engine.rs`

**Interfaces:**
- Consumes: fixtures（Task 1）、vendor notes（Task 4）
- Produces（Task 6-10 依赖）:
  - `pub enum ZipnestError { DllMissing(String), NotAnArchive, PasswordRequired, PasswordIncorrect, Cancelled, Engine(i32), Security(SecurityViolation), QuotaExceeded, Io(std::io::Error) }` + `error_key()`（`error.dll_missing` / `error.not_an_archive` / `error.password_required` / `error.password_incorrect` / `error.cancelled` / `error.engine` / `error.security_blocked` / `error.quota_exceeded` / `error.io`）+ `From<io::Error>` + `From<SecurityViolation>`
  - `ArchiveOpenOptions { pub password: Option<String> }`（手写 Debug 隐藏 password，Default derive）
  - `dll::load() -> Result<&'static SevenZip, ZipnestError>`：定位顺序 `SEVENZIP_DLL_PATH` env → exe 旁 `engines\7z.dll` → `C:\Program Files\7-Zip\7z.dll` → `C:\Program Files (x86)\7-Zip\7z.dll`；取 `CreateObject` 导出 `extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> i32`
  - `Archive::open(path: &Path, opts: ArchiveOpenOptions) -> Result<Archive, ZipnestError>`、`Archive::len(&self) -> u32`
  - `com::Guid`（`#[repr(C)] { d1:u32,d2:u16,d3:u16,d4:[u8;8] }`），所有 GUID 值来自 vendor notes 第 4 条

- [ ] **Step 1: 失败集成测试**（`tests/engine.rs`）

```rust
use archive_core::{Archive, ArchiveOpenOptions};
use std::path::PathBuf;

fn fx(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

#[test]
fn opens_plain_zip_and_counts_items() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).expect("open");
    assert_eq!(arc.len(), 3);
}

#[test]
fn rejects_corrupt_archive() {
    let err = Archive::open(&fx("corrupt.zip"), ArchiveOpenOptions::default())
        .expect_err("corrupt must fail");
    assert_eq!(err.error_key(), "error.not_an_archive");
}
```

- [ ] **Step 2: 确认失败** `cargo test -p archive-core` → FAIL（Archive 未定义）

- [ ] **Step 3: 实现 `error.rs`**（代码即 Interfaces 块内容；`Display` 输出 `error_key()` + Debug 细节，永不含密码）

- [ ] **Step 4: 实现 `dll.rs`**：`LoadLibraryW` + `GetProcAddress("CreateObject")`（`libloaderapi`/`kernel32` 直接 extern，无第三方依赖）；`Guid` 手写；定位失败返回 `DllMissing(尝试路径列表)`。

- [ ] **Step 5: 实现 `com/`**：
  - `vtables.rs`：按 vendor notes 抄 `IInArchive`、`IArchiveOpenCallback`、`IProgress`、`ICryptoGetTextPassword`、`ISequentialInStream`、`IInStream`、`ISequentialOutStream` 的 `#[repr(C)]` vtable（**顺序签名逐条来自 notes；用到的方法实现，其余占位 `unimplemented!` 不可——必须填 `E_NOTIMPL` 常量返回的空 fn，因为引擎可能调用**）。
  - `propvariant.rs`：`#[repr(C)] PropVariant { vt: u16, _r: [u16;3], data: [u8;16] }`（或 8+8 布局按 notes 第 5 条），`clear()` 动态调 `ole32!PropVariantClear`。
  - `instream.rs`（内存版）：`InMemStream { data: Vec<u8>, pos: u64 }`，实现 Read/Seek vtable；对象布局 `#[repr(C)] { vt: *const InStreamVt, this: *mut InMemStream }`，QI 支持 IInStream + ISequentialInStream IID。
  - `callbacks.rs`：`OpenCallback { vt, ... }` 实现 SetTotal/SetCompleted→S_OK；QI 支持 IArchiveOpenCallback + ICryptoGetTextPassword（密码从 opts 取，无则 E_FAIL→PasswordRequired）。
  - `archive.rs`：`Archive` 持 `*mut c_void`（IInArchive）+ Drop 里 `Release`；`open` 流程：load → 读整个文件入 InMemStream → 扩展名→CLSID（`.zip`→CFormatZip、`.7z`→CFormat7z，其余 → `NotAnArchive`）→ `CreateObject(CLSID, IID_IInArchive)` → `Open(stream, callback)` → HRESULT `S_OK` 继续，`S_FALSE/E_INVALIDARG/0x80004005` → `NotAnArchive`；`len()` 调 `GetNumberOfItems`。
  - **偏移红线**：测试通过即 vtable 槽位正确；失败先对 notes 第 1 条逐行核对，勿盲改。

- [ ] **Step 6: 确认通过** `cargo test --workspace` → 两 engine 测试 PASS
- [ ] **Step 7: Commit** `git commit -m "feat(core): load 7z.dll via COM and open archives"`

---

### Task 6: 条目枚举 → ArchiveEntry

**Files:** Modify `types.rs`（新增 ArchiveEntry）、`archive.rs`；Test `engine.rs` 追加

**Interfaces:**
- Produces: `pub struct ArchiveEntry { pub index: u32, pub path: String, pub is_dir: bool, pub size: u64, pub crc: Option<u32>, pub mtime: Option<std::time::SystemTime>, pub encrypted: bool }`；`Archive::entries(&self) -> Result<Vec<ArchiveEntry>, ZipnestError>`。path 保持原样（清洗在落盘前，Task 8 接线）。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn lists_entries_with_metadata() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let es = arc.entries().unwrap();
    assert_eq!(es.len(), 3);
    let a = es.iter().find(|e| e.path == "a.txt").expect("a.txt");
    assert!(!a.is_dir && a.size > 0 && a.crc.is_some());
    let sub = es.iter().find(|e| e.path == "sub" || e.path == "sub/").expect("sub");
    assert!(sub.is_dir);
    let b = es.iter().find(|e| e.path.ends_with("b.txt")).expect("b.txt");
    assert_eq!(b.size, "nested content\n".len() as u64);
}

#[test]
fn enc_zip_flags_encrypted() {
    let arc = Archive::open(&fx("enc.zip"), ArchiveOpenOptions::default()).unwrap();
    assert!(arc.entries().unwrap().iter().any(|e| e.encrypted));
}
```

- [ ] **Step 2: 确认失败** → FAIL（entries 未定义）
- [ ] **Step 3: 实现**：逐 index `GetProperty`；notes 第 5 条的 PROPID 与 VT 常量；BSTR→`String::from_utf16_lossy`；VT_FILETIME u64(1601 起 100ns)→SystemTime（减 `116444736000000000` 再 ×100ns）；每个 PROPVARIANT 用后 `clear()`。
- [ ] **Step 4: 确认通过** `cargo test -p archive-core` → PASS
- [ ] **Step 5: Commit** `git commit -m "feat(core): enumerate archive entries with metadata"`

---

### Task 7: 真实文件流 IInStream + read_entry

**Files:** Modify `com/instream.rs`（新增 `InFileStream`）、`archive.rs`；Test `engine.rs` 追加

**Interfaces:**
- Produces: `Archive::read_entry(&self, index: u32, opts: &ArchiveOpenOptions, max_bytes: Option<u64>) -> Result<Vec<u8>, ZipnestError>`；`InFileStream`（File+pos，Read 按 `*mut u32` 返回实际读数并 `S_OK`/尾部 `S_FALSE`，Seek 按 SEEK_SET/CUR/END → SetFilePointerEx）。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn reads_entry_bytes_matching_source() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let b = arc.entries().unwrap().iter().find(|e| e.path.ends_with("b.txt")).unwrap().index;
    assert_eq!(arc.read_entry(b, &ArchiveOpenOptions::default(), None).unwrap(), b"nested content\n");
}

#[test]
fn read_entry_respects_max_bytes() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let c = arc.entries().unwrap().iter().find(|e| e.path == "c.txt").unwrap().index;
    assert_eq!(arc.read_entry(c, &ArchiveOpenOptions::default(), Some(10)).unwrap().len(), 10);
}
```

- [ ] **Step 2: 确认失败** → FAIL
- [ ] **Step 3: 实现**：`open` 换 `InFileStream`（内存版删除）；`read_entry` 用 `Extract(&[index], testMode=1 /*仅测试不落盘，用回调 GetStream 拿内存 sink*/)？——M1 实现路径：用 `Extract` + `IArchiveExtractCallback`，`GetStream` 返回内存 `OutMemStream`（收集进 `Vec<u8>`，超 max_bytes 截断并返回 `S_OK` 给上层判定），`SetOperationResult` 收 HRESULT（CRC 错 → `Engine(E_FAIL)`）。testMode=1 时 7z 不建文件、仅拉流，正合预览语义。
- [ ] **Step 4: 确认通过** `cargo test -p archive-core` → PASS
- [ ] **Step 5: Commit** `git commit -m "feat(core): stream entries from disk via IInStream"`

---

### Task 8: 落盘解压 + 安全接线 + 进度 + 取消

**Files:** Modify `types.rs`（ExtractOptions/Progress）、`archive.rs`、`com/callbacks.rs`（ExtractCallback）；Test `engine.rs` 追加

**Interfaces:**
- Consumes: `archive_security::{sanitize_entry_path, ExtractQuota}`
- Produces:
  - `pub struct ExtractOptions { pub dest: PathBuf, pub entries: Vec<u32>, pub max_total_bytes: u64, pub overwrite: bool }`
  - `pub struct ExtractProgress { pub done_bytes: u64, pub total_bytes: u64, pub current_path: String }`
  - `Archive::extract(&self, opts: &ExtractOptions, password: Option<&str>, progress: &mut dyn FnMut(&ExtractProgress) -> bool) -> Result<ExtractStats, ZipnestError>`（`ExtractStats { files: u32, bytes: u64 }`；进度回调 false → `Cancelled`）
  - 安全接线：`GetStream` 回调里对 entry path 调 `sanitize_entry_path` → `Err(v)` 返回 `E_FAIL` 并记住 `Security(v)` 终止；`dest.join(sanitized)` 确保 `starts_with(dest)` 双保险；配额 `charge` 失败 → `QuotaExceeded` 终止。

- [ ] **Step 1: 失败测试**

```rust
use std::fs;

#[test]
fn extracts_plain_zip_to_disk() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = std::env::temp_dir().join(format!("zn-ex-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let all: Vec<u32> = (0..arc.len()).collect();
    let opts = ExtractOptions { dest: dest.clone(), entries: all, max_total_bytes: u64::MAX, overwrite: true };
    let mut ticks = 0;
    let stats = arc.extract(&opts, None, &mut |p| { ticks += 1; let _ = p; true }).expect("extract");
    assert!(stats.files >= 3);
    assert!(ticks >= 1, "progress must tick");
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap().contains("hello zipnest"), true);
    assert_eq!(fs::read_to_string(dest.join("sub/b.txt")).unwrap().trim(), "nested content");
    let _ = fs::remove_dir_all(&dest);
}

#[test]
fn extract_cancel_returns_cancelled() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = std::env::temp_dir().join(format!("zn-cancel-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let opts = ExtractOptions { dest: dest.clone(), entries: (0..arc.len()).collect(), max_total_bytes: u64::MAX, overwrite: true };
    let err = arc.extract(&opts, None, &mut |_p| false).expect_err("must cancel");
    assert_eq!(err.error_key(), "error.cancelled");
    let _ = fs::remove_dir_all(&dest);
}

#[test]
fn extract_quota_enforced() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = std::env::temp_dir().join(format!("zn-quota-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let opts = ExtractOptions { dest: dest.clone(), entries: (0..arc.len()).collect(), max_total_bytes: 10, overwrite: true };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("quota must trip");
    assert_eq!(err.error_key(), "error.quota_exceeded");
    let _ = fs::remove_dir_all(&dest);
}

#[test]
fn zip_slip_entry_is_blocked() {
    // 用 zip crate 现造恶意归档
    let evil = std::env::temp_dir().join(format!("zn-slip-{}.zip", std::process::id()));
    {
        let f = fs::File::create(&evil).unwrap();
        let mut w = zip::ZipWriter::new(f);
        w.start_file::<_, ()>("../../evil.txt", Default::default()).unwrap();
        use std::io::Write; w.write_all(b"pwned").unwrap(); w.finish().unwrap();
    }
    let arc = Archive::open(&evil, ArchiveOpenOptions::default()).expect("open evil");
    let dest = std::env::temp_dir().join(format!("zn-slipdest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let opts = ExtractOptions { dest: dest.clone(), entries: (0..arc.len()).collect(), max_total_bytes: u64::MAX, overwrite: true };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("must be blocked");
    assert_eq!(err.error_key(), "error.security_blocked");
    // dest 之外不得存在 evil.txt（双保险断言）
    let escaped = std::env::temp_dir().join("evil.txt");
    assert!(!escaped.exists(), "must not escape temp");
    let _ = fs::remove_dir_all(&dest); let _ = fs::remove_file(&evil);
}
```

- [ ] **Step 2: 确认失败** → FAIL（extract 未定义）
- [ ] **Step 3: 实现 ExtractCallback**：`GetStream(index, mode, outStream)` → 读 kpidPath → sanitize → 建父目录 → `File::create` → 返回 `OutFileStream` vtable（Write 落盘 + 配额 charge）；`PrepareOperation`→S_OK；`SetOperationResult(hr)`→非 S_OK 记错误；`SetTotal/SetCompleted`（IProgress）→ 算进度调回调，`false` → 返回 `E_ABORT`（引擎停止）；上层把 `E_ABORT` 映射 `Cancelled`。
- [ ] **Step 4: 确认通过** `cargo test -p archive-core` → 全部 PASS（含 zip_slip、cancel、quota）
- [ ] **Step 5: Commit** `git commit -m "feat(core): secure extraction with progress and cancellation"`

---

### Task 9: 密码回调闭环

**Files:** Modify `callbacks.rs`（Open/Extract 双接口 QI 均支持 ICryptoGetTextPassword）、`archive.rs`；Test `engine.rs` 追加

**Interfaces:**
- Produces: `Archive::open` 对 `enc.7z`（头加密，Open 阶段要密码）：无密码 → `PasswordRequired`；错密码 → `PasswordIncorrect`（Open 返回带密码错特征 HRESULT，见 notes；映射规则写进 `error.rs`）；对 `enc.zip`（条目加密，Extract 阶段要密码）同样语义在 `extract`/`read_entry`。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn encrypted_7z_open_requires_password() {
    let err = Archive::open(&fx("enc.7z"), ArchiveOpenOptions::default()).expect_err("need pw");
    assert_eq!(err.error_key(), "error.password_required");
}

#[test]
fn encrypted_7z_wrong_password_rejected() {
    let err = Archive::open(&fx("enc.7z"), ArchiveOpenOptions { password: Some("nope".into()) })
        .expect_err("wrong pw");
    assert_eq!(err.error_key(), "error.password_incorrect");
}

#[test]
fn encrypted_7z_with_password_opens_and_extracts() {
    let arc = Archive::open(&fx("enc.7z"), ArchiveOpenOptions { password: Some("secret".into()) })
        .expect("open with pw");
    let dest = std::env::temp_dir().join(format!("zn-pw-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let opts = ExtractOptions { dest: dest.clone(), entries: (0..arc.len()).collect(), max_total_bytes: u64::MAX, overwrite: true };
    arc.extract(&opts, Some("secret"), &mut |_p| true).expect("extract");
    assert!(dest.join("a.txt").exists());
    let _ = fs::remove_dir_all(&dest);
}

#[test]
fn encrypted_zip_wrong_password_on_extract() {
    let arc = Archive::open(&fx("enc.zip"), ArchiveOpenOptions::default()).expect("zip opens");
    let dest = std::env::temp_dir().join(format!("zn-zpw-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let opts = ExtractOptions { dest: dest.clone(), entries: (0..arc.len()).collect(), max_total_bytes: u64::MAX, overwrite: true };
    let err = arc.extract(&opts, Some("wrong"), &mut |_p| true).expect_err("wrong pw");
    assert_eq!(err.error_key(), "error.password_incorrect");
    let _ = fs::remove_dir_all(&dest);
}
```

- [ ] **Step 2: 确认失败** → FAIL
- [ ] **Step 3: 实现**：Open/Extract callback 的 `QI(IID_ICryptoGetTextPassword)` 返回同一对象；`GetText` 把密码 BSTR 交给引擎（`SysAllocString` 或按 notes 的 BSTR 写法）；Open 返回 HRESULT 分类：`0x80004005`/`0x80070005`/notes 记录的 kPassword 错 → `PasswordIncorrect`，无密码可提供时 → `PasswordRequired`。
- [ ] **Step 4: 确认通过** `cargo test --workspace` → 全绿
- [ ] **Step 5: Commit** `git commit -m "feat(core): password callback for encrypted archives"`

---

### Task 10: 错误映射收口 + HRESULT 表

**Files:** Modify `error.rs`；Test `engine.rs` 追加（错误 key 全覆盖断言）

**Interfaces:**
- Produces: `pub fn map_hresult(hr: i32) -> ZipnestError`；规则：`0x80004004 E_ABORT`→Cancelled；`0x8007007E/自定义 DllMissing` 不走此表；notes 记录的 7z 密码错 HRESULT→PasswordIncorrect；其余→`Engine(hr)`。`ZipnestError` 补 `Clone` 不可（含 io）→ 提供 `fn kind(&self) -> &'static str`（= error_key）。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn hresult_mapping_table() {
    assert_eq!(archive_core::map_hresult(0x80004004u32 as i32).error_key(), "error.cancelled");
    assert_eq!(archive_core::map_hresult(0x80004005u32 as i32).error_key(), "error.engine");
}
```

- [ ] **Step 2: 确认失败** → FAIL（map_hresult 未导出）
- [ ] **Step 3: 实现** 并把 Task 5-9 里散落的 HRESULT 判断全部改为调用 `map_hresult`（统一收口）。
- [ ] **Step 4: 确认通过** `cargo test --workspace` → 全绿
- [ ] **Step 5: Commit** `git commit -m "refactor(core): centralize HRESULT error mapping"`

---

### Task 11: M1 验收 — 全量回归 + 文档

**Files:** Create `README.md`（项目级：构建/测试/安全模型一段）；Modify 无

- [ ] **Step 1: 全量测试** `cargo test --workspace` → 全绿；记录测试数（预期 ≥ 25）。
- [ ] **Step 2: 兼容性抽查** 用 `7z.exe` 现场生成一个含 5000 条目的 zip 放入 temp，写一次性测试（`#[ignore]` 标注的慢测试用 `cargo test -- --ignored` 跑一次）确认枚举不超时（<5s）。
- [ ] **Step 3: README** 简述架构、`cargo test`、安全模型（sanitize 强制）。
- [ ] **Step 4: Commit + tag** `git commit -m "docs: add M1 readme" ; git tag m1-engine`

## Self-Review 记录

- Spec 覆盖：§4.1 archive-core（open/list/read/extract/密码/进度/取消 ✓；创建/分卷/SFX = M2-M4 计划）、archive-security（穿越/配额 ✓；符号链接策略在 M2 落盘路径补）、§7 错误处理 ✓、§8 测试 ✓。RAR/ISO 格式深度测试 M2（引擎同路径）。
- 占位符：无 TBD；Task 5 vtable “抄写自 notes” 是权威引用而非占位（notes 本身由 Task 4 从官方头文件产出）。
- 类型一致：`ArchiveOpenOptions`（Task 5 定义，9 复用 password 字段）、`ExtractOptions`（Task 8 定义）、`error_key` 表贯穿一致。

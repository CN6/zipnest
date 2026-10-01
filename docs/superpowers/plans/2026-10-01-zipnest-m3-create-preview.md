# ZipNest M3 — 创建与预览 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 ZipNest 加上**创建归档**（ZIP/7Z/TAR 家族，含等级/方法/AES-256/文件名加密/分卷/SFX）与**归档内预览**（文本/图片/hex）两大能力，全部经现有 IPC 三层暴露，测试绿。

**Architecture:** 引擎先行，小步切片。在 `archive-core` 扩出 `ArchiveWriter`（对标既有 `Archive`），复用现有 `com/` 手工 COM vtable 模式，走 `IOutArchive` + `IArchiveUpdateCallback` + `IOutStream`；随后接 `archive-jobs`（新增 `create` 任务类型）与 `zipnest-ipc`（`create_archive`），最后做前端创建向导与预览面板。**不改动 M1/M2 已交付的任何行为**。

**Tech Stack:** Rust 1.98 / edition 2021 / msvc；7-Zip 26.03 动态加载的 `7z.dll`（`IOutArchive`）；SDK 头文件已在 `vendor/7zip-sdk/`；前端 React 18 + TS + Fluent 2；测试用本机 `7z.exe` 交叉验证产物。

**Spec:** `docs/superpowers/specs/2026-09-30-zipnest-design.md`（§4.2 create-wizard / preview-panel，§6 IPC 契约，§9 打包）

## Global Constraints

- 平台：仅 Windows（x64 开发/测试；ARM64 打包为 M4 项）。PowerShell 5.1：每个新 shell 先 `[Console]::OutputEncoding=[Text.Encoding]::UTF8` 且 `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`。
- RAR：只解压、永不创建。本计划不含任何 RAR 创建代码。
- `7z.dll`：仅动态加载（LGPL）；禁静态链接、禁改 DLL；新增 vendor 的 SFX stub 必须附其 license 文本。
- 密码：仅内存传递；`CreateOptions`/`ArchiveOpenOptions` 手写 `Debug` 脱敏；禁止进日志。
- 错误：跨层只允许 `ZipnestError`（`error_key()` 返回 `"error.xxx"`）或 `IpcError { key }`；引擎禁止 panic 外泄到 IPC。
- 创建的每个输出路径写入前经 `archive_security` 校验（拒绝 `..`/绝对路径/盘符/UNC/保留名）；源枚举默认跳过符号链接。
- 进度回调返回 `false` = 取消 → 引擎尽快中止并返回 `ZipnestError::Cancelled`。
- i18n：每个新增 key 必须同时有 zh-CN 与 en-US 文案（vitest 有双向校验）。
- 每任务结束：`cargo test --workspace` 全绿 + `pnpm test` 通过 + clippy 无警告 + 英文 commit。

---

## File Structure

```
crates/archive-core/
├── src/
│   ├── create.rs              # 新增：ArchiveWriter + create_archive 驱动
│   ├── types.rs               # 修改：CreateFormat/Level/Method/SfxKind/CreateOptions/CreateSource/CreateProgress/CreateStats
│   ├── lib.rs                 # 修改：导出新类型与 create 模块
│   └── com/
│       ├── mod.rs             # 修改：新增 IID_IOUT_ARCHIVE / IID_ISET_PROPERTIES / IID_IOUT_STREAM / IID_IARCHIVE_UPDATE_CALLBACK(2)
│       ├── vtables.rs         # 修改：新增 OutArchiveVt / SetPropertiesVt / OutStreamVt / UpdateCallbackVt
│       ├── propvariant.rs     # 修改：新增 setter（from_bstr/from_u64/from_bool/from_filetime/from_empty）
│       ├── outstream.rs       # 新增：IOutStream 文件实现（支持分卷滚动）
│       └── outcallback.rs     # 新增：IArchiveUpdateCallback + ICryptoGetTextPassword
├── tests/create.rs            # 新增：创建引擎集成测试（真实 7z.dll）
├── vendor/7zip-sfx/           # 新增（Task 8）：7z.sfx / 7zCon.sfx + license
crates/archive-jobs/src/lib.rs # 修改（如需）：create 任务类型
crates/zipnest-ipc/src/{service.rs,registry.rs}  # 修改：create_archive + 归档格式记录
apps/zipnest/src-tauri/src/commands.rs           # 修改：create_archive 命令
apps/zipnest/src/components/CreateWizard.tsx      # 新增
apps/zipnest/src/components/PreviewPanel.tsx      # 新增
apps/zipnest/src/hooks/useArchive.ts              # 修改：创建 + 预览接线
apps/zipnest/src/i18n/locales/{en-US,zh-CN}.json  # 修改：新增 key
fixtures/make_fixtures.ps1                        # 修改：新增创建测试用的源树
docs/superpowers/specs/2026-09-30-zipnest-design.md # 修改：M3 范围（去掉任务中心）
README.md                                        # 修改：M3 状态
```

---

### Task 1: ArchiveWriter 骨架 — 把单个文件写进 ZIP 与 7Z（Copy 方式）

**Files:**
- Create: `crates/archive-core/src/com/outstream.rs`, `crates/archive-core/src/com/outcallback.rs`, `crates/archive-core/src/create.rs`
- Modify: `crates/archive-core/src/com/mod.rs`（新增 IID 常量）, `crates/archive-core/src/com/vtables.rs`（新增 vtable 结构）, `crates/archive-core/src/com/propvariant.rs`（新增 setter）, `crates/archive-core/src/types.rs`（新增创建类型）, `crates/archive-core/src/lib.rs`（导出）
- Test: `crates/archive-core/tests/create.rs`

**Interfaces:**
- Consumes: 既有 `dll::load()` / `SevenZip::create_object`、`com::{Guid,Hresult,S_OK,E_FAIL,ComObject,qi_matches,as_void}`、`com::instream::FileStreamOwner`、`com::callbacks::crypto_new`、`error::ZipnestError`。
- Produces:
  - `types.rs`: `CreateFormat { Zip, SevenZ, Tar, TarGz, TarBz2, TarXz }`；`CompressionLevel { Store, Fastest, Normal, Maximum, Ultra }`（`fn engine_x(&self) -> &'static str`）；`CompressionMethod { Auto, Copy, Deflate, Lzma2, Bzip2 }`（`fn engine_name(&self) -> Option<&'static str>`）；`SfxKind { Gui, Console }`；`CreateOptions { format, level, method, password: Option<String>, encrypt_names: bool, volume_bytes: Option<u64>, sfx: Option<SfxKind> }`（**派生 `Clone`**，手写 `Debug` 脱敏）；`CreateSource { path: PathBuf, node: String }`；`CreateProgress { done_bytes, total_bytes, current_path }`；`CreateStats { files: u32, bytes_in: u64, bytes_out: u64 }`（`Default`+`Clone`+`Copy`）。所有枚举派生 `Debug`+`Clone`+`Copy`+`PartialEq`+`Eq`。
  - `create.rs`: `pub fn create_archive(sources: &[CreateSource], dest: &Path, opts: &CreateOptions, progress: &mut dyn FnMut(&CreateProgress) -> bool) -> Result<CreateStats, ZipnestError>`（Task 1 只实现 `level=Store`+`method=Copy`、`password=None`、`volume_bytes=None`、`sfx=None` 的分支）。
  - `com/mod.rs`: `IID_IOUT_ARCHIVE`, `IID_ISET_PROPERTIES`, `IID_IOUT_STREAM`, `IID_IARCHIVE_UPDATE_CALLBACK`, `IID_IARCHIVE_UPDATE_CALLBACK2`。

- [ ] **Step 1: 新增 IID 常量**

在 `crates/archive-core/src/com/mod.rs` 的 IID 区块追加（GUID 规则同文件已有 `guid7`，ARCHIVE 组字节 = 6）：

```rust
pub const IID_IOUT_STREAM: Guid = guid7(3, 0x04);
pub const IID_IOUT_ARCHIVE: Guid = guid7(6, 0xA0);
pub const IID_ISET_PROPERTIES: Guid = guid7(6, 0x03);
pub const IID_IARCHIVE_UPDATE_CALLBACK: Guid = guid7(6, 0x80);
pub const IID_IARCHIVE_UPDATE_CALLBACK2: Guid = guid7(6, 0x82);
```

（依据：`vendor/7zip-sdk/Archive/IArchive.h` 中 `Z7_IFACE_CONSTR_ARCHIVE(IOutArchive, 0xA0)`、`..._ARCHIVE(ISetProperties, 0x03)`、`..._SUB(IArchiveUpdateCallback, IProgress, 0x80)`；`vendor/7zip-sdk/IStream.h` 中 `IOutStream` = 组 3、子 0x04。）

- [ ] **Step 2: 新增 vtable 结构**

在 `crates/archive-core/src/com/vtables.rs` 末尾追加（先加 `OutStreamVt` 与 `OutArchiveVt`；`SetPropertiesVt`/`UpdateCallbackVt` 在 Task 1 需要用到 UpdateItems，但回调接口在 Step 4 定义，故此处一并加上）：

```rust
/// `IOutStream` = IUnknown + Write + Seek + SetSize（vendor `IStream.h`）。
#[repr(C)]
pub struct OutStreamVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub write: unsafe extern "system" fn(
        this: *mut c_void,
        data: *const c_void,
        size: u32,
        processed: *mut u32,
    ) -> Hresult,
    pub seek: unsafe extern "system" fn(
        this: *mut c_void,
        offset: i64,
        origin: u32,
        new_position: *mut u64,
    ) -> Hresult,
    pub set_size: unsafe extern "system" fn(this: *mut c_void, new_size: u64) -> Hresult,
}

/// `IOutArchive` = IUnknown + UpdateItems + GetFileTimeType。
#[repr(C)]
pub struct OutArchiveVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub update_items: unsafe extern "system" fn(
        this: *mut c_void,
        out_stream: *mut c_void,
        num_items: u32,
        update_callback: *mut c_void,
    ) -> Hresult,
    pub get_file_time_type: unsafe extern "system" fn(this: *mut c_void, t: *mut u32) -> Hresult,
}

/// `ISetProperties` = IUnknown + SetProperties。
#[repr(C)]
pub struct SetPropertiesVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    pub set_properties: unsafe extern "system" fn(
        this: *mut c_void,
        names: *const *const u16,
        values: *const PropVariant,
        num_props: u32,
    ) -> Hresult,
}
```

- [ ] **Step 3: 给 PropVariant 加 setter**

`com/propvariant.rs` **已有** `empty()`、`clear()`（`vt==VT_BSTR` 时经 `PropVariantClear` 释放）、`alloc_bstr(s)`（`SysAllocStringLen`）与常量 `VT_*`、`FILETIME_UNIX_EPOCH_DELTA`。仅需在同一 `impl` 内追加 setter（复用 `alloc_bstr`，无需新增 link）：

```rust
impl PropVariant {
    /// kpidPath / 字符串属性：BSTR 由 PropVariant 持有，clear() 释放。
    pub fn from_bstr(s: &str) -> Self {
        PropVariant { vt: VT_BSTR, reserved1: 0, reserved2: 0, reserved3: 0, data: alloc_bstr(s) as u64 }
    }
    pub fn from_u64(v: u64) -> Self {
        PropVariant { vt: VT_UI8, reserved1: 0, reserved2: 0, reserved3: 0, data: v }
    }
    pub fn from_bool(v: bool) -> Self {
        PropVariant { vt: VT_BOOL, reserved1: 0, reserved2: 0, reserved3: 0, data: (if v { -1i32 } else { 0 }) as u32 as u64 }
    }
    pub fn from_filetime(t: std::time::SystemTime) -> Self {
        let ticks = t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64 / 100 + FILETIME_UNIX_EPOCH_DELTA)
            .unwrap_or(FILETIME_UNIX_EPOCH_DELTA);
        PropVariant { vt: VT_FILETIME, reserved1: 0, reserved2: 0, reserved3: 0, data: ticks }
    }
}
```

（`empty()` 已存在，勿重复定义。）

- [ ] **Step 4: 实现 `IOutStream` 与 `IArchiveUpdateCallback`**

`com/outstream.rs`：结构 `FileOutStream { vt, refs, file: Mutex<File>, written: u64 }`，`write` 落盘并累加 `written`，`seek`/`set_size` 直通；`FileStreamOwner` 的 `as_void/into_raw/release_own` 模式照抄（见 `instream.rs`）。文件末尾提供 `pub fn new(path: &Path) -> std::io::Result<*mut c_void>`（`Box::into_raw`，初始 `refs=1`）与 `pub unsafe fn release_void(p: *mut c_void)`。

`com/outcallback.rs`：结构 `UpdateCallback { vt, refs, items: Vec<SourceItem>, state: Arc<UpdateState>, crypto: *mut c_void }`，其中
```rust
pub struct SourceItem { pub path: std::path::PathBuf, pub node: String, pub is_dir: bool, pub size: u64, pub mtime: Option<std::time::SystemTime> }
pub struct UpdateState { pub done_bytes: AtomicU64, pub total_bytes: u64, pub cancelled: AtomicU8, pub last_path: Mutex<String>, pub progress: Mutex<ProgressCell>, pub io_error: Mutex<Option<std::io::Error>> }
```
vtable `UpdateCallbackVt`（IUnknown + 6 方法，顺序照 `vendor/7zip-sdk/Archive/IArchive.h` 的 `IArchiveUpdateCallback`）：
```rust
#[repr(C)]
pub struct UpdateCallbackVt {
    pub query_interface: QueryInterfaceFn,
    pub add_ref: AddRefFn,
    pub release: ReleaseFn,
    // IProgress（子接口，ID 0x80 继承）：SetTotal / SetCompleted
    pub set_total: unsafe extern "system" fn(this: *mut c_void, total: u64) -> Hresult,
    pub set_completed: unsafe extern "system" fn(this: *mut c_void, complete: *const u64) -> Hresult,
    // IArchiveUpdateCallback 自身
    pub get_update_item_info: unsafe extern "system" fn(
        this: *mut c_void, index: u32,
        new_data: *mut i32, new_props: *mut i32, index_in_archive: *mut u32,
    ) -> Hresult,
    pub get_property: unsafe extern "system" fn(
        this: *mut c_void, index: u32, prop_id: u32, value: *mut PropVariant,
    ) -> Hresult,
    pub get_stream: unsafe extern "system" fn(
        this: *mut c_void, index: u32, in_stream: *mut *mut c_void,
    ) -> Hresult,
    pub set_operation_result: unsafe extern "system" fn(this: *mut c_void, op_res: i32) -> Hresult,
}
```
行为要点：
- `get_update_item_info`：`*new_data=1, *new_props=1, *index_in_archive=u32::MAX`（全部新建）。
- `get_property`：`kpidPath`→`from_bstr(&node)`；`kpidIsDir`→`from_bool(is_dir)`；`kpidSize`→`from_u64(size)`；`kpidMTime`→`from_filetime(mtime)`；其余→`empty()`。
- `get_stream`：目录→`*in_stream=null, S_OK`；文件→`FileStreamOwner::new(File::open(path))` 的裸指针。
- `set_total`：记录 `total_bytes`（若尚未预扫描则由调用方设好）；`set_completed`：更新 `done_bytes`，调进度回调，返回 `false` 时置 `cancelled` 并返回 `E_ABORT`。
- `query_interface`：`IID_IUNKNOWN`/`IID_IPROGRESS`/`IID_IARCHIVE_UPDATE_CALLBACK`→自身；`IID_ICRYPTO_GET_TEXT_PASSWORD`→透传 `crypto`（复用 `callbacks::qi_hand_out_crypto` 的写法）。

- [ ] **Step 5: 实现 `create_archive` 驱动（Task 1 子集）**

`create.rs`：定位 CLSID（`CreateFormat::Zip→CLSID_FORMAT_ZIP`，`SevenZ→CLSID_FORMAT_7Z`），`dll.create_object(clsid, IID_IOUT_ARCHIVE, &mut raw)`；`ArchiveWriter::create` 流程：建 `FileOutStream`（`dest`）→ 建 `UpdateCallback`（items）→（Task 1 先跳过 SetProperties）→ `OutArchiveVt.update_items(out_stream, n, cb)` → 收尾 release。落盘后 `CreateStats { files, bytes_in, bytes_out: dest 文件大小 }`。错误经 `map_hresult`；`io_error`/`cancelled` 先行判定（照 `extract_to_disk` 的顺序）。**本步同时**：在 `types.rs` 加入上述创建类型（枚举派生 `Debug+Clone+Copy+PartialEq+Eq`；`CreateOptions` 派生 `Clone`、手写 `Debug` 脱敏 `password`）；在 `lib.rs` 加 `pub mod create;` 并导出 `create::{create_archive, collect_sources}`（`collect_sources` 在 Task 3 加，Task 1 可先 `pub use create::create_archive;`）及新类型。

- [ ] **Step 6: 失败测试 → 实现 → 通过**

`crates/archive-core/tests/create.rs`：
```rust
use archive_core::{create::{create_archive, CreateSource}, Archive, ArchiveOpenOptions, types::{CreateFormat, CompressionLevel, CompressionMethod, CreateOptions}};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf { std::env::temp_dir().join(format!("zn-create-{}-{}", std::process::id(), name)) }

fn write_src(dir: &std::path::Path, rel: &str, body: &[u8]) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

fn opts(fmt: CreateFormat) -> CreateOptions {
    CreateOptions { format: fmt, level: CompressionLevel::Store, method: CompressionMethod::Copy,
        password: None, encrypt_names: false, volume_bytes: None, sfx: None }
}

#[test]
fn creates_zip_with_one_file_roundtrip() {
    let base = tmp("zip"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"hello create");
    let dest = base.join("out.zip");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut n = 0u64;
    create_archive(&srcs, &dest, &opts(CreateFormat::Zip), &mut |_| { n += 1; true }).unwrap();
    assert!(dest.exists(), "output zip must exist");
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let es = arc.entries().unwrap();
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].path, "a.txt");
    let bytes = arc.read_entry(0, &ArchiveOpenOptions::default(), None).unwrap();
    assert_eq!(bytes, b"hello create");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_7z_with_one_file_roundtrip() {
    let base = tmp("7z"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"seven zip body");
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    create_archive(&srcs, &dest, &opts(CreateFormat::SevenZ), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    assert_eq!(arc.read_entry(0, &ArchiveOpenOptions::default(), None).unwrap(), b"seven zip body");
    let _ = std::fs::remove_dir_all(&base);
}
```
Run: `cargo test -p archive-core --test create` → 初始应编译失败（`create` 模块/类型未定义）→ 实现 Step 1–5 → 全绿。
交叉验证（可选）：`& "C:\Program Files\7-Zip\7z.exe" t <dest>` 报 `Everything is Ok`。

- [ ] **Step 7: Commit** `git add -A; git commit -m "feat(engine): ArchiveWriter writes single-file ZIP/7Z"`

---

### Task 2: 压缩等级与方法（SetProperties）

**Files:**
- Modify: `crates/archive-core/src/create.rs`, `crates/archive-core/src/types.rs`（`CompressionLevel::engine_x` / `CompressionMethod::engine_name` 已在 Task 1 定义，此处接上）
- Test: `crates/archive-core/tests/create.rs`

**Interfaces:**
- Consumes: Task 1 的 `create_archive` 与类型。
- Produces: `create_archive` 支持 `level != Store` 与 `method != Auto` 时，QueryInterface 到 `ISetProperties` 并设 `x`/`m`。

- [ ] **Step 1: 失败测试**

追加到 `tests/create.rs`：
```rust
#[test]
fn deflate_compresses_repetitive_zip() {
    let base = tmp("defl"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.txt", &vec![b'A'; 200_000]);
    let store = base.join("store.zip"); let deflate = base.join("deflate.zip");
    let srcs = vec![CreateSource { path: src.clone(), node: "big.txt".into() }];
    let mut o = opts(CreateFormat::Zip); o.level = CompressionLevel::Normal; o.method = CompressionMethod::Deflate;
    create_archive(&srcs, &store, &{ let mut s = o.clone(); s.level = CompressionLevel::Store; s.method = CompressionMethod::Copy; s }, &mut |_| true).unwrap();
    create_archive(&srcs, &deflate, &o, &mut |_| true).unwrap();
    assert!(std::fs::metadata(&deflate).unwrap().len() * 4 < std::fs::metadata(&store).unwrap().len(),
        "deflate must be far smaller than store on repetitive data");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn lzma2_is_default_small_for_7z() {
    let base = tmp("lz"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.txt", &vec![b'B'; 200_000]);
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "big.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.level = CompressionLevel::Normal; o.method = CompressionMethod::Lzma2;
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(std::fs::metadata(&dest).unwrap().len() < 20_000, "LZMA2 must compress repetitive data well");
    let _ = std::fs::remove_dir_all(&base);
}
```
Run → FAIL。

- [ ] **Step 2: 实现 SetProperties**

在 `create.rs` 的 `create_archive` 内、`update_items` 之前调用：
```rust
/// Best-effort archive-level properties via ISetProperties. Handlers that
/// don't implement the interface are simply left at defaults.
unsafe fn apply_properties(raw: *mut c_void, opts: &CreateOptions) {
    let mut sp: *mut c_void = std::ptr::null_mut();
    let oav = &**(raw as *const *const crate::com::vtables::OutArchiveVt);
    if (oav.query_interface)(raw, &crate::com::IID_ISET_PROPERTIES, &mut sp) != crate::com::S_OK
        || sp.is_null()
    {
        return;
    }
    // Wide names must outlive the call; keep the backing Vec<u16> alive.
    let mut names_keep: Vec<Vec<u16>> = Vec::new();
    let mut names: Vec<*const u16> = Vec::new();
    let mut values: Vec<PropVariant> = Vec::new();

    fn add(name: &str, pv: PropVariant,
           keep: &mut Vec<Vec<u16>>, names: &mut Vec<*const u16>, values: &mut Vec<PropVariant>) {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        keep.push(w);
        names.push(keep.last().unwrap().as_ptr());
        values.push(pv);
    }

    add("x", PropVariant::from_bstr(opts.level.engine_x()), &mut names_keep, &mut names, &mut values);
    if let Some(m) = opts.method.engine_name() {
        add("m", PropVariant::from_bstr(m), &mut names_keep, &mut names, &mut values);
    }
    // Task 6 追加：if opts.encrypt_names && opts.format == CreateFormat::SevenZ { add("he", from_bstr("on"), ...) }
    // Task 7 追加：if let Some(v) = opts.volume_bytes { add("v", from_bstr(&v.to_string()), ...) }

    let sp_vt = &**(sp as *const *const crate::com::vtables::SetPropertiesVt);
    (sp_vt.set_properties)(sp, names.as_ptr(), values.as_ptr(), names.len() as u32);
    (sp_vt.release)(sp);
    for v in values.iter_mut() { v.clear(); } // 释放 BSTR 载荷
}
```
实现要点：`SetProperties` 的名称是 7-Zip 的短开关名（`x`=等级、`m`=方法、`he`=头部加密、`v`=分卷）；`names`/`values` 与 `names_keep` 同作用域，避免悬垂；结束后逐个 `clear()` 释放 BSTR。

- [ ] **Step 3: 通过 + 交叉验证** Run 测试全绿；可选 `7z l -slt` 看 `Method = Deflate`/`LZMA2`。

- [ ] **Step 4: Commit** `git commit -am "feat(engine): compression level and method for create"`

---

### Task 3: 多源与目录递归（保留相对结构）

**Files:**
- Modify: `crates/archive-core/src/create.rs`（新增源预处理）
- Test: `crates/archive-core/tests/create.rs`

**Interfaces:**
- Produces: `pub fn collect_sources(inputs: &[PathBuf], base: &Path) -> Result<Vec<CreateSource>, ZipnestError>`：目录递归展开为逐文件条目 + 目录条目；`node` = 相对 `base` 的 `/` 分隔路径；跳过符号链接（`symlink_metadata` 判定）。

- [ ] **Step 1: 失败测试**
```rust
#[test]
fn collects_directory_tree_preserving_structure() {
    let base = tmp("tree"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/a.txt", b"aaa");
    write_src(&base, "src/sub/b.txt", b"bbb");
    let dest = base.join("out.zip");
    let sources = archive_core::create::collect_sources(&[base.join("src")], &base).unwrap();
    create_archive(&sources, &dest, &opts(CreateFormat::Zip), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let paths: Vec<String> = arc.entries().unwrap().iter().map(|e| e.path.replace('\\', "/")).collect();
    assert!(paths.contains(&"src/a.txt".to_string()));
    assert!(paths.contains(&"src/sub/b.txt".to_string()));
    assert!(paths.iter().any(|p| p == "src/sub/" || p == "src/sub"));
    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2: 实现 `collect_sources`**（递归 `read_dir`，`node = rel.replace('\\', "/")`；目录也发条目以保证空目录可见；`symlink_metadata` 为 symlink 时跳过）

- [ ] **Step 3: 通过 + Commit** `git commit -am "feat(engine): recursive sources with relative structure"`

---

### Task 4: TAR 家族（tar / tar.gz / tar.bz2 / tar.xz）

**Files:**
- Modify: `crates/archive-core/src/create.rs`（按 `format` 选 CLSID 与两级编码）
- Test: `crates/archive-core/tests/create.rs`

**Interfaces:**
- Consumes: `CreateFormat::{Tar,TarGz,TarBz2,TarXz}`。
- Produces: 这些格式的 `create_archive` 分支。

- [ ] **Step 1: 失败测试**
```rust
#[test]
fn creates_targz_roundtrip() {
    let base = tmp("tgz"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/a.txt", b"tar gz body");
    let dest = base.join("out.tar.gz");
    let sources = archive_core::create::collect_sources(&[base.join("src")], &base).unwrap();
    create_archive(&sources, &dest, &opts(CreateFormat::TarGz), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| !e.is_dir).expect("file entry");
    assert!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap().starts_with(b"tar gz body"));
    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2: 实现**：`Tar` 直接写；`TarGz/TarBz2/TarXz` 先写临时 `.tar`，再用 `CLSID_FORMAT_GZIP/BZIP2/XZ` 的 `IOutArchive` 把 tar 包一层（第二遍 UpdateItems，源 = 该 tar 文件，node = `dest` 的文件名去掉 `.gz/.bz2/.xz`）。完成后删除临时 tar。

- [ ] **Step 3: 通过 + Commit** `git commit -am "feat(engine): create TAR and compressed TAR variants"`

---

### Task 5: 密码 AES-256（ZIP 与 7Z）

**Files:**
- Modify: `crates/archive-core/src/create.rs`, `crates/archive-core/src/com/outcallback.rs`
- Test: `crates/archive-core/tests/create.rs`

**Interfaces:**
- Consumes: `callbacks::crypto_new(password)`（既有）。
- Produces: `password: Some(_)` 时向引擎提供 `ICryptoGetTextPassword`；ZIP 走 AES-256（设 `m` 保持 Deflate/LZMA2，引擎默认 AES-256）。

- [ ] **Step 1: 失败测试**
```rust
#[test]
fn creates_encrypted_zip_needs_password() {
    let base = tmp("encz"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"secret body");
    let dest = base.join("enc.zip");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::Zip); o.password = Some("pw123".into());
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    // 无密码打开 → 条目应标记 encrypted
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    assert!(arc.entries().unwrap().iter().any(|e| e.encrypted));
    // 正确密码可读回
    let arc = Archive::open(&dest, ArchiveOpenOptions { password: Some("pw123".into()) }).unwrap();
    assert_eq!(arc.read_entry(0, &ArchiveOpenOptions { password: Some("pw123".into()) }, None).unwrap(), b"secret body");
    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2: 实现**：`UpdateCallback` 构造时把 `crypto_new(password)` 挂上；`query_interface` 透传 `IID_ICRYPTO_GET_TEXT_PASSWORD`。测试红→绿。

- [ ] **Step 3: 通过 + Commit** `git commit -am "feat(engine): AES-256 password for create (zip/7z)"`

---

### Task 6: 7z 文件名加密（he）

**Files:** Modify `create.rs`；Test `tests/create.rs`

- [ ] **Step 1: 失败测试**
```rust
#[test]
fn creates_header_encrypted_7z_hides_names() {
    let base = tmp("he"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"hidden");
    let dest = base.join("he.7z");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.password = Some("pw".into()); o.encrypt_names = true;
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    let err = Archive::open(&dest, ArchiveOpenOptions::default()).expect_err("no password");
    assert_eq!(err.error_key(), "error.password_required");
    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2: 实现**：`encrypt_names && format==SevenZ` 时 `SetProperties` 追加 `("he", PropVariant::from_bstr("on"))`；`format==Zip` 时忽略（UI 侧已置灰）。

- [ ] **Step 3: 通过 + Commit** `git commit -am "feat(engine): 7z header encryption for create"`

---

### Task 7: 分卷（7z 与 ZIP）— 先 spike 再实现

**Files:** Modify `create.rs`, `com/outstream.rs`（分卷滚动）；Test `tests/create.rs`

> **为何 spike：** `IOutArchive` 的分卷机制（`v` 属性 vs 由调用方的 out stream 滚动）在 SDK 头文件里未直接给出，需实证确定。Spike 的产出是**结论**，不是保留代码。

- [ ] **Step 1: Spike — 判定机制**
实验脚本（临时测试，允许丢弃）：用 `SetProperties([("v", "1m")])` + 单文件 `FileOutStream` 创建 >1MB 的 7z，观察：(a) 是否报错；(b) 是否只生成 `out.7z`（未分卷）；(c) 是否由引擎请求多流。记录结论到本任务下方注释。
Run: | `cargo test -p archive-core --test create spike_volume_probe -- --nocapture`
Expected: 得出"分卷由谁负责"的结论（文档化）。

- [ ] **Step 2: 失败测试（依 Spike 结论）** 若结论是"调用方 out stream 滚动"：
```rust
#[test]
fn splits_7z_into_volumes() {
    let base = tmp("vol"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(base.join("vol.7z.001").exists(), "first volume must exist");
    assert!(base.join("vol.7z.002").exists(), "second volume must exist");
    // 首卷可被打开（7z 支持从 .001 打开多卷）
    let arc = Archive::open(&base.join("vol.7z.001"), ArchiveOpenOptions::default()).unwrap();
    assert!(arc.entries().unwrap().iter().any(|e| e.path.ends_with("big.bin")));
    let _ = std::fs::remove_dir_all(&base);
}
```
（ZIP 分卷同法，断言 `vol.zip.001`。）

- [ ] **Step 3: 实现**：`volume_bytes.is_some()` 时，`FileOutStream` 变为"分卷 out stream"——写满一卷即切换到 `dest.{:03}` 的下一卷（ZIP 分卷若引擎要求 `.zip.001` 命名，按引擎期望命名）。命名：7z→`<stem>.7z.{:03}`；zip→`<stem>.zip.{:03}`。

- [ ] **Step 4: 通过 + 交叉验证** `7z t vol.7z.001` 报 OK。

- [ ] **Step 5: Commit** `git commit -am "feat(engine): split volumes for 7z and zip"`

---

### Task 8: 自解压 SFX — 先 spike 再实现

**Files:** Create `vendor/7zip-sfx/{7z.sfx,7zCon.sfx,LICENSE.txt}`；Modify `create.rs`；Test `tests/create.rs`

> **为何 spike：** SFX 的拼接方式（stub + archive 的偏移是否被 7z 识别）与 stub 架构（x64/ARM64）需实证。产出是结论。

- [ ] **Step 1: 归档 stub（分发物料）**
复制 `C:\Program Files\7-Zip\7z.sfx` 与 `7zCon.sfx` 到 `vendor/7zip-sfx/`，并写 `README.md` 注明来源（7-Zip 26.03）与 license（LGPL）。核对两 stub 的 PE 架构（`dumpbin /headers` 或读 `MZ`/`PE` 头），记录 x64/ARM64 可得性。
Run: `Get-Item vendor\7zip-sfx\*.sfx | Select Name,Length`

- [ ] **Step 2: Spike — 验证拼接可被读取**
手工把一个已建好的 `x.7z` 与 `vendor/7zip-sfx/7zCon.sfx` 拼接：
```powershell
cmd /c copy /b "vendor\7zip-sfx\7zCon.sfx" + "x.7z" "x.exe"
& "C:\Program Files\7-Zip\7z.exe" l "x.exe"
```
Expected: `7z l x.exe` 能列出 `x.7z` 的条目（证明 7z 识别带前导 stub 的归档）。记录结论。

- [ ] **Step 3: 失败测试**
```rust
#[test]
fn creates_7z_sfx_runnable() {
    let base = tmp("sfx"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"sfx body");
    let dest = base.join("packed.exe");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.sfx = Some(SfxKind::Console);
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    // 产物以 MZ 开头，且 7z 能读取其内嵌归档
    let head = std::fs::read(&dest).unwrap();
    assert_eq!(&head[0..2], b"MZ", "SFX must be a PE executable");
    let out = std::process::Command::new("C:\\Program Files\\7-Zip\\7z.exe").arg("l").arg(&dest).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("a.txt"), "7z must find the embedded archive");
    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 4: 实现**：`sfx.is_some()` 时，先按普通 7z 建到临时 `tmp.7z`，再把 `stub + tmp.7z` 拼接写入 `dest`（`.exe`）；stub 依 `SfxKind` 选 `7z.sfx`/`7zCon.sfx`，路径从 `vendor/7zip-sfx/`（运行时 = exe 同级的 `engines/sfx/`）。完成删临时文件。若 Spike 证明拼接不可读，则改为在 `FileOutStream` 前预写 stub 并让引擎从偏移写（记录改动）。

- [ ] **Step 5: 通过 + Commit** `git commit -am "feat(engine): 7z self-extracting archives (sfx)"`

---

### Task 9: 创建的进度与取消

**Files:** Modify `create.rs`, `com/outcallback.rs`；Test `tests/create.rs`

- [ ] **Step 1: 失败测试**
```rust
#[test]
fn create_progress_is_monotonic_and_cancel_stops() {
    let base = tmp("prog"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![7u8; 3_000_000]);
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut seen = 0u64;
    let mut o = opts(CreateFormat::SevenZ); o.level = CompressionLevel::Normal;
    create_archive(&srcs, &dest, &o, &mut |p| { seen += 1; assert!(p.done_bytes <= p.total_bytes.max(p.done_bytes)); true }).unwrap();
    let _ = std::fs::remove_dir_all(&base);

    let dest2 = base.join("cancel.7z");
    let err = create_archive(&srcs, &dest2, &o, &mut |_| false).expect_err("cancel must abort");
    assert_eq!(err.error_key(), "error.cancelled");
    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2: 实现**：预扫描源求 `total_bytes`；`set_completed`/`set_total` 驱动进度；回调返回 `false` → `cancelled=1` 且回调返回 `E_ABORT`；`create_archive` 收尾先判 `cancelled` → `ZipnestError::Cancelled`。

- [ ] **Step 3: 通过 + Commit** `git commit -am "feat(engine): create progress and cancellation"`

---

### Task 10: IPC — `create_archive` 服务与任务

**Files:** Modify `crates/archive-jobs/src/lib.rs`（若需注册 `create`）, `crates/zipnest-ipc/src/service.rs`, `crates/zipnest-ipc/src/registry.rs`; Test `crates/zipnest-ipc/tests/service.rs`

**Interfaces:**
- Consumes: `archive_core::create::{create_archive, collect_sources}`、`CreateOptions` 等。
- Produces: `IpcService::create_archive(&self, sources: Vec<String>, dest: String, opts: CreateRequest) -> Result<u64, IpcError>`；`CreateRequest`（IPC DTO，`format: String` 等）序列化；成功/失败发 `job_finished`（复用既有事件）。

- [ ] **Step 1: 失败测试**（`tests/service.rs`，复用 `fx`/`service`/`wait_events` helper）
```rust
#[test]
fn create_archive_emits_finished_ok() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zn-ipc-create-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("a.txt"); std::fs::write(&src, b"ipc create").unwrap();
    let dest = tmp.join("out.zip");
    let job = svc.create_archive(vec![src.to_string_lossy().into()], dest.to_string_lossy().into(),
        serde_json::from_value(serde_json::json!({"format":"zip","level":"normal","method":"auto"})).unwrap()).unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], true, "{:?}", fin.1);
    drop(v);
    assert!(dest.exists());
    let _ = std::fs::remove_dir_all(&tmp);
}
```

- [ ] **Step 2: 实现**：`service.create_archive` 走 `jobs.submit("create", ...)`，runner 内 `collect_sources` + `create_archive`，进度经 `ctx.report`，取消经 `ctx.cancelled()`。

- [ ] **Step 3: 通过 + Commit** `git commit -am "feat(ipc): create_archive service and job"`

---

### Task 11: Tauri 命令 + 创建向导 UI

**Files:** Modify `apps/zipnest/src-tauri/src/commands.rs`, `apps/zipnest/src-tauri/src/lib.rs`（注册 handler）, `apps/zipnest/src/hooks/useArchive.ts`, `apps/zipnest/src/App.tsx`, `apps/zipnest/src/i18n/locales/{en-US,zh-CN}.json`; Create `apps/zipnest/src/components/CreateWizard.tsx`

**Interfaces:**
- Consumes: `invoke("create_archive", ...)`。
- Produces: 工具栏“新建压缩包”入口 → `CreateWizard`（源列表、目标、格式、等级、方法、密码、加密文件名、分卷、SFX）→ 调用 command → 进度复用 `job_progress`/`job_finished`。

- [ ] **Step 1: 命令**（照既有命令风格）`#[tauri::command] pub fn create_archive(state: State<IpcService>, sources: Vec<String>, dest: String, options: serde_json::Value) -> Result<u64, IpcError>`；在 `lib.rs` handler 列表注册。
- [ ] **Step 2: 向导组件 + i18n**：新增 `create.*` keys（两语齐全）；`CreateWizard.tsx` 用 Fluent `Dialog`/`Dropdown`/`Input`；SFX 勾选时强制 7z 且加密文件名在 zip 下置灰。
- [ ] **Step 3: 接线**：`useArchive.ts` 加 `startCreate()`；App 工具栏按钮。
- [ ] **Step 4: 验证 + Commit** `pnpm build` + `pnpm test` 绿；`git commit -am "feat(web): create wizard"`

---

### Task 12: 预览面板（文本/图片/hex）

**Files:** Create `apps/zipnest/src/components/PreviewPanel.tsx`; Modify `useArchive.ts`, `App.tsx`, i18n

**Interfaces:**
- Consumes: 既有 `read_entry_bytes(archive_id, entry, max_bytes)` command。
- Produces: 选中单文件条目时右侧预览：文本（UTF-8/GBK/UTF-16 探测，限 256KB，只读 `<pre>`）、图片（png/jpg/gif/bmp/webp，`<img>` blob URL）、其余 hex（前 4KB）；不可预览 → “用外部程序打开”（复用 `open_entry`）。

- [ ] **Step 1: 编码探测纯函数**（`previewCodec.ts`）+ vitest：BOM 检测（UTF-8/UTF-16LE/BE）、GBK 启发式（非法 UTF-8 序列→按 GBK 解）→ `{ kind: 'text'|'image'|'hex'|'external', ... }`。
- [ ] **Step 2: 组件 + 接线**：`useArchive.ts` 加 `previewEntry(index)`；`App.tsx` 在有选中文件时渲染面板。
- [ ] **Step 3: i18n + 验证 + Commit**：`pnpm build`/`pnpm test` 绿；`git commit -am "feat(web): archive entry preview panel"`

---

### Task 13: 全量回归 + 文档 + tag

**Files:** Modify `README.md`, `docs/superpowers/specs/2026-09-30-zipnest-design.md`

- [ ] **Step 1: spec/README 更新**：spec §11 M3 去掉“任务中心”，写明创建能力清单与预览范围；README 的 M3 状态与“Not yet”列表同步。
- [ ] **Step 2: 全量回归**：`cargo test --workspace`（应含新增 create 测试）+ `cargo clippy --workspace --all-targets`（零警告）+ `pnpm test` + `pnpm build`。
- [ ] **Step 3: 实机冒烟**：`tauri dev` 手工走一遍：新建 zip（含目录）、新建 7z+密码、新建分卷、预览文本/图片。
- [ ] **Step 4: Commit + tag** `git commit -am "docs: m3 status"`; `git tag m3-create-preview`

---

## Self-Review Checklist（写计划时已核对，执行时再核对一次）

- [ ] 每个 spec 的 M3 需求都有对应任务：创建格式（T1/T4）、等级方法（T2）、目录（T3）、AES-256（T5）、文件名加密（T6）、分卷（T7）、SFX（T8）、进度取消（T9）、IPC（T10）、向导（T11）、预览（T12）。
- [ ] 无占位符：所有 spike 步骤都给了确切的实验命令与"产出＝结论"的界定；类型/签名前后一致（`CreateOptions`/`CreateSource`/`create_archive`/`collect_sources` 全程同名同型）。
- [ ] 硬约束贯穿：密码脱敏（T1 类型 + T5）、动态 7z.dll（T1）、安全校验（T3 源、T11 目标）、错误键（全程 `error.*`）、i18n 双语（T11/T12）。
- [ ] **任务中心已移除**；settings/关联/托盘/安装器留在 M4。
```

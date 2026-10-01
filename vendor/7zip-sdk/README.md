# vendor/7zip-sdk

Vendored **7-Zip 26.03** C++ headers, used **only as the authoritative source
for hand-written Rust FFI** (vtable slot order, IIDs/CLSIDs, PROPVARIANT layout).
ZipNest loads `7z.dll` dynamically at runtime (LGPL); no code from these headers
is compiled or linked.

- Source: https://github.com/ip7z/7zip (tag `26.03`)
- License: see `LICENSE` (7-Zip license, same terms as 7z.dll redistribution)
- Local machine 7z.dll: `C:\Program Files\7-Zip\7z.dll` (7-Zip 26.03 x64)

Files: `Archive/IArchive.h`, `IProgress.h`, `IStream.h`, `ICoder.h`,
`IPassword.h`, `IDecl.h`, `PropID.h`, `Guid.txt`, `RegisterArc.h`,
`RegisterCodec.h`, `Archive/7z/7zRegister.cpp`, `Archive/ZipRegister.cpp`
(actually `ZipHandler.cpp`).

---

## Rust FFI notes (authoritative for `archive-core/src/com/*`)

### 1. Exported factory

```c
typedef HRESULT (WINAPI *Func_CreateObject)(const GUID *clsID, const GUID *iid, void **outObject);
```

`extern "system"` (stdcall on x64 = fastcall-compatible, use `"system"`).
Export name: `CreateObject`.

### 2. GUID construction (IDecl.h + Guid.txt)

All 7-Zip GUIDs: `Data1=0x23170F69, Data2=0x40C1, Data3=0x278A`,
`Data4 = [0, 0, 0, groupId, 0, subId, 0, 0]` (bytes), except handler CLSIDs:

- **Interfaces**: `{23170F69-40C1-278A-0000-00GG00SS0000}`
  where `GG` = groupId (hex), `SS` = subId (hex).
- **Handler CLSIDs**: `{23170F69-40C1-278A-1000-000110xx0000}` (Guid.txt,
  "Handler GUIDs" table).

Values needed by ZipNest:

| Name | groupId:subId / xx | GUID |
|---|---|---|
| `IID_IUnknown` | (standard COM) | `{00000000-0000-0000-C000-000000000046}` |
| `IID_IProgress` | 00:05 | `{23170F69-40C1-278A-0000-000000050000}` |
| `IID_ISequentialInStream` | 03:01 | `{23170F69-40C1-278A-0000-000300010000}` |
| `IID_ISequentialOutStream` | 03:02 | `{23170F69-40C1-278A-0000-000300020000}` |
| `IID_IInStream` | 03:03 | `{23170F69-40C1-278A-0000-000300030000}` |
| `IID_ICryptoGetTextPassword` | 05:10 | `{23170F69-40C1-278A-0000-000500100000}` |
| `IID_IArchiveOpenCallback` | 06:10 | `{23170F69-40C1-278A-0000-000600100000}` |
| `IID_IArchiveExtractCallback` | 06:20 | `{23170F69-40C1-278A-0000-000600200000}` |
| `IID_IInArchive` | 06:60 | `{23170F69-40C1-278A-0000-000600600000}` |
| `CLSID_CFormatZip` | xx=01 | `{23170F69-40C1-278A-1000-000110010000}` |
| `CLSID_CFormat7z` | xx=07 | `{23170F69-40C1-278A-1000-000110070000}` |
| `CLSID_CFormatRar` | xx=03 | `{23170F69-40C1-278A-1000-000110030000}` |
| `CLSID_CFormatRar5` | xx=CC | `{23170F69-40C1-278A-1000-000110CC0000}` |
| `CLSID_CFormatTar` | xx=EE | `{23170F69-40C1-278A-1000-000110EE0000}` |
| `CLSID_CFormatGZip` | xx=EF | `{23170F69-40C1-278A-1000-000110EF0000}` |
| `CLSID_CFormatXZ` | xx=0C | `{23170F69-40C1-278A-1000-0001100C0000}` |
| `CLSID_CFormatIso` | xx=E7 | `{23170F69-40C1-278A-1000-000110E70000}` |
| `CLSID_CFormatBZip2` | xx=02 | `{23170F69-40C1-278A-1000-000110020000}` |

(Other formats: see `Guid.txt`; gzip contains tar — use CLSID_CFormatGZip for
`.tar.gz`, CLSID_CFormatTar for plain `.tar`.)

### 3. Vtable layout rules

- All methods are `HRESULT __stdcall` (`extern "system"`).
- Every interface starts with IUnknown: `QueryInterface`, `AddRef`, `Release`.
- Interface inheritance = flattened single vtable:
  - `ISequentialInStream` = IUnknown + `Read`
  - `IInStream` = ISequentialInStream + `Seek` (base methods first)
  - `IProgress` = IUnknown + `SetTotal`, `SetCompleted`
  - `IArchiveExtractCallback` = **IProgress** + `GetStream`,
    `PrepareOperation`, `SetOperationResult` (base vtable first!)
  - `IArchiveOpenCallback` = IUnknown + `SetTotal`, `SetCompleted`
    (note: does NOT derive IProgress in the macro — `Z7_IFACE_CONSTR_ARCHIVE(IArchiveOpenCallback, 0x10)`)

### 4. Interface methods (exact order)

**IInArchive** (IArchive.h `Z7_IFACEM_IInArchive`), slots after IUnknown:

| # | Method | Signature |
|---|---|---|
| 0 | Open | `(IInStream*, const UInt64* maxCheckStartPosition, IArchiveOpenCallback*) -> HRESULT` |
| 1 | Close | `() -> HRESULT` |
| 2 | GetNumberOfItems | `(UInt32* numItems) -> HRESULT` |
| 3 | GetProperty | `(UInt32 index, PROPID propID, PROPVARIANT* value) -> HRESULT` |
| 4 | Extract | `(const UInt32* indices, UInt32 numItems, Int32 testMode, IArchiveExtractCallback*) -> HRESULT` |
| 5 | GetArchiveProperty | `(PROPID, PROPVARIANT*) -> HRESULT` |
| 6 | GetNumberOfProperties | `(UInt32*) -> HRESULT` |
| 7 | GetPropertyInfo | `(UInt32 index, BSTR*, PROPID*, VARTYPE*) -> HRESULT` |
| 8 | GetNumberOfArchiveProperties | `(UInt32*) -> HRESULT` |
| 9 | GetArchivePropertyInfo | `(UInt32 index, BSTR*, PROPID*, VARTYPE*) -> HRESULT` |

- `Extract`: `indices` sorted ascending; `numItems = 0xFFFFFFFF` = all;
  `testMode != 0` = don't write to outStream (but GetStream still called with
  askExtractMode; returning `*outStream == NULL` skips the file).
- `Open`: `maxCheckStartPosition = NULL` allows signature search anywhere.

**IArchiveOpenCallback**: `SetTotal(const UInt64* files, const UInt64* bytes)`,
`SetCompleted(const UInt64* files, const UInt64* bytes)`.

**IProgress**: `SetTotal(UInt64 total)`, `SetCompleted(const UInt64* completeValue)`.

**IArchiveExtractCallback** (= IProgress slots +):

| # | Method | Signature |
|---|---|---|
| +0 | GetStream | `(UInt32 index, ISequentialOutStream** outStream, Int32 askExtractMode) -> HRESULT` |
| +1 | PrepareOperation | `(Int32 askExtractMode) -> HRESULT` |
| +2 | SetOperationResult | `(Int32 opRes) -> HRESULT` |

- `askExtractMode`: 0=kExtract, 1=kTest, 2=kSkip, 3=kReadExternal (NAskMode).
- `GetStream` returns: `S_OK` ok, `S_FALSE` data error (decoders);
  `*outStream == NULL` → skip file (directories/links).
- `SetOperationResult(opRes)`: `NOperationResult`: 0=OK, 1=UnsupportedMethod,
  2=DataError, 3=CRCError, 4=Unavailable, 5=UnexpectedEnd, 6=DataAfterEnd,
  7=IsNotArc, 8=HeadersError, **9=WrongPassword**.
- Progress callbacks may arrive **on another thread** concurrently with
  IArchiveExtractCallback methods → our callback struct needs interior
  synchronization (RefCell is NOT enough for progress vs GetStream; use
  `Send`-safe state or complete progress on one thread only — see note in
  IArchive.h lines 183-191).

**ICryptoGetTextPassword**: `(BSTR* password) -> HRESULT`. Input BSTR is NULL;
we allocate output with `SysAllocString`; caller frees with `SysFreeString`
(exported by oleaut32.dll).

**ISequentialInStream::Read**: `(void* data, UInt32 size, UInt32* processedSize)`
— partial reads allowed; must read ≥1 byte if data available; if position past
end → `*processedSize = 0`, `S_OK`.

**IInStream::Seek**: `(Int64 offset, UInt32 seekOrigin, UInt64* newPosition)`
— origins 0/1/2 = SET/CUR/END; seek before 0 → error
`__HRESULT_FROM_WIN32(ERROR_NEGATIVE_SEEK)` = 0x80070083.

**ISequentialOutStream::Write**: `(const void* data, UInt32 size, UInt32* processedSize)`
— partial writes allowed.

### 5. PROPVARIANT (Windows ABI, x64)

```text
offset 0:  vt: u16            // VARTYPE
offset 2:  wReserved1: u16    // 7-Zip: time precision level for VT_FILETIME
offset 4:  wReserved2: u16
offset 6:  wReserved3: u16
offset 8:  data: 8 bytes      // union payload (pointer-sized)
offset 16: tail padding: 8 bytes
total 24 bytes on x64
```

On x64 the `PROPVARIANT` union contains `DECIMAL` (16 bytes), so
`sizeof(PROPVARIANT) == 24`, NOT 16 (16 is the x86 size). This matters for
**arrays** of variants (e.g. `ISetProperties`): the engine indexes with a
24-byte stride, so a 16-byte Rust struct misreads every element after the
first. The Rust `PropVariant` in `crates/archive-core/src/com/propvariant.rs`
therefore carries an 8-byte tail.

- Init to `vt = VT_EMPTY (0)` before every call: the callee may
  `VariantClear()` the input value (IArchive.h lines 30-41).
- Free after use: `PropVariantClear` (ole32.dll, `extern "system"`).

Payload decode (little-endian):

- `VT_BSTR (8)`: `bstrVal = data as usize as *mut u16` → UTF-16 with 32-bit
  length prefix at `ptr-4`; simplest read: walk until 0 or use length prefix.
  Free with `SysFreeString` (if 7-Zip allocated it — GetProperty strings are
  7-Zip-allocated BSTRs).
- `VT_BOOL (11)`: `boolVal` = i32 at data: `-1` (VARIANT_TRUE) / 0.
- `VT_UI4 (19)`: u32 at data.
- `VT_I8 (20)` / `VT_UI8 (21)`: u64.
- `VT_FILETIME (64)`: two u32 `{low, high}` → u64 100ns since 1601-01-01.
- `VT_EMPTY (0)`: no value.

### 6. PROPID values (PropID.h enum, counted)

| Name | Value | Typical vt |
|---|---|---|
| kpidPath | 3 | VT_BSTR |
| kpidIsDir | 6 | VT_BOOL |
| kpidSize | 7 | VT_UI8 |
| kpidPackSize | 8 | VT_UI8 |
| kpidMTime | 12 | VT_FILETIME |
| kpidEncrypted | 15 | VT_BOOL |
| kpidCRC | 19 | VT_UI4 |
| kpidMethod | 22 | VT_BSTR |
| kpidPhySize | 44 | VT_UI8 |
| kpidErrorType | 69 | VT_UI4 |
| kpidErrorFlags | 71 | VT_UI4 |
| kpidWarningFlags | 72 | VT_UI4 |
| kpidWarning | 73 | VT_BSTR |

`kpv_ErrorFlags` (kpidErrorFlags bits): 1=IsNotArc, 2=HeadersError,
**4=EncryptedHeadersError**, 8=UnavailableStart, 0x20=UnexpectedEnd,
0x400=CrcError, ...

### 7. HRESULT constants

`S_OK=0`, `S_FALSE=1`, `E_NOTIMPL=0x80004001`, `E_NOINTERFACE=0x80004002`,
`E_ABORT=0x80004004`, `E_FAIL=0x80004005`, `E_ACCESSDENIED=0x80070005`,
`E_INVALIDARG=0x80070057`, `E_OUTOFMEMORY=0x8007000E`,
`HRESULT_FROM_WIN32(ERROR_NEGATIVE_SEEK)=0x80070083`.

### 8. Wrong-password detection strategy

- Header-encrypted 7z (`enc.7z`): `Open` fails; query
  `GetArchiveProperty(kpidErrorFlags)` on the still-attached handler:
  bit 4 (`EncryptedHeadersError`) set → if we supplied a password →
  `PasswordIncorrect`; if we had none → `PasswordRequired`. (Open error alone
  cannot distinguish wrong password from not-an-archive.)
- Entry-encrypted zip (`enc.zip`): open succeeds; wrong password surfaces via
  `SetOperationResult(9)` (kWrongPassword) or `GetStream` returning S_FALSE
  with opRes=9 → map to `PasswordIncorrect`.
- If `GetTextPassword` is called but no password is available, return
  `E_FAIL` (0x80004005) from our callback.

### 9. Thread-safety contract

`IInArchive` methods must NOT be called concurrently on the same object
(IArchive.h notes). But `IProgress::{SetTotal,SetCompleted}` MAY arrive from
another thread during `Extract` → progress callback state must be
thread-safe (`std::sync::Mutex`/atomics), `GetStream`/`PrepareOperation`/
`SetOperationResult` stay single-threaded per the header.

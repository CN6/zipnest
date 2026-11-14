//! Archive creation driver (M3).
//!
//! Task 1 implemented the `Store` + `Copy` subset (no password, no volumes, no
//! SFX) for ZIP and 7Z. Task 2 wires `ISetProperties` so the requested
//! compression level (`x`) and method (`m`) reach the handler. Task 4 adds the
//! TAR family: plain TAR writes directly through `CLSID_FORMAT_TAR`, while
//! `TarGz`/`TarBz2`/`TarXz` first write a temporary `.tar` and then wrap it with
//! the GZIP/BZIP2/XZ handler. Task 5 adds AES-256 password creation: the
//! password is handed to the engine through `ICryptoGetTextPassword2` on the
//! update callback, and ZIP additionally forces AES-256 via the `em` property.
//! Task 7 adds split volumes: for ZIP/7z the output is written to
//! `<dest>.001`, `.002`, ... through the multi-volume stream in `outstream`,
//! because the handler only ever sees a single stream (7-Zip's own client
//! splits the same way). Task 8 adds self-extracting archives: a 7z build is
//! prepended with a vendored SFX stub, producing a runnable `<stub> + <7z>`
//! `.exe`.

use crate::com::callbacks;
use crate::com::outcallback::{self, SourceItem, UpdateState};
use crate::com::outstream;
use crate::com::propvariant::PropVariant;
use crate::com::vtables::{OutArchiveVt, SetPropertiesVt};
use crate::com::{
    Guid, S_OK, CLSID_FORMAT_7Z, CLSID_FORMAT_BZIP2, CLSID_FORMAT_GZIP, CLSID_FORMAT_TAR,
    CLSID_FORMAT_XZ, CLSID_FORMAT_ZIP, E_FAIL, E_INVALIDARG, E_NOTIMPL, IID_IOUT_ARCHIVE,
    IID_ISET_PROPERTIES,
};
use crate::dll;
use crate::error::ZipnestError;
use crate::types::{
    CompressionMethod, CreateFormat, CreateOptions, CreateProgress, CreateStats, SfxKind,
    SkippedLink,
};
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::types::CreateSource;

/// [`collect_sources_detailed`] 的结果：进包的条目，加上被跳过的链接。
///
/// 两者必须一起交给调用方。只拿到 `sources` 的话，「目录里只有链接」会表现
/// 成一个空包：引擎照样返回成功，用户却什么都没拿到（违反 AGENTS.md 的
/// 「Nothing is ever lost」精神）。`Default` + `Extend` 是为了让多输入的调用
/// 方逐个收集再合并，不必自己写拼接。
#[derive(Debug, Clone, Default)]
pub struct CollectedSources {
    /// 真正会写进归档的条目（目录与文件，按遍历顺序）。
    pub sources: Vec<CreateSource>,
    /// 因为是指向别处的链接而没有跟进的条目。
    pub skipped_links: Vec<SkippedLink>,
}

impl Extend<CollectedSources> for CollectedSources {
    fn extend<T: IntoIterator<Item = CollectedSources>>(&mut self, iter: T) {
        for part in iter {
            self.sources.extend(part.sources);
            self.skipped_links.extend(part.skipped_links);
        }
    }
}

/// Expand `inputs` into a flat item table, walking directories recursively so a
/// directory tree can be added while preserving its structure.
///
/// `node` is each item's path relative to `base`, always `/`-separated.
/// Directory entries are emitted too (so empty directories stay visible).
/// Symbolic links — files or directories, and junctions with them — are skipped
/// rather than followed. This wrapper drops the skipped-link record; callers
/// that show the user what was left out must use [`collect_sources_detailed`].
pub fn collect_sources(
    inputs: &[PathBuf],
    base: &Path,
) -> Result<Vec<CreateSource>, ZipnestError> {
    Ok(collect_sources_detailed(inputs, base)?.sources)
}

/// Same walk as [`collect_sources`], but also reports every link that was left
/// out (see [`CollectedSources::skipped_links`]).
pub fn collect_sources_detailed(
    inputs: &[PathBuf],
    base: &Path,
) -> Result<CollectedSources, ZipnestError> {
    let mut out = CollectedSources::default();
    for input in inputs {
        collect_one(input, base, &mut out)?;
    }
    Ok(out)
}

/// The safe archive node for `path`, relative to `base`.
///
/// Returns `Ok(None)` when `path` *is* `base` (no name of its own), and an
/// error when `path` is not lexically under `base` or the relative name is
/// unsafe (absolute/traversal/reserved). Callers must never write an
/// unsanitized name into the archive (zip-slip).
fn rel_node(path: &Path, base: &Path) -> Result<Option<String>, ZipnestError> {
    let rel = path.strip_prefix(base).map_err(|_| {
        ZipnestError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "source {} is not under base {}",
                path.display(),
                base.display()
            ),
        ))
    })?;
    if rel.as_os_str().is_empty() {
        return Ok(None);
    }
    let raw = rel.to_string_lossy().replace('\\', "/");
    Ok(Some(archive_security::sanitize_entry_path(&raw)?))
}

fn collect_one(
    path: &Path,
    base: &Path,
    out: &mut CollectedSources,
) -> Result<(), ZipnestError> {
    let meta = std::fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        // 链接不跟进（理由见 `SkippedLink`），但必须记账：一个只装着链接的
        // 目录否则会变成「空包 + 成功」。`is_dir` 用跟随后的元数据判定，
        // 悬空链接读不到目标，按文件计。
        let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
        out.skipped_links.push(SkippedLink {
            path: path.to_path_buf(),
            is_dir,
        });
        return Ok(());
    }
    if meta.is_dir() {
        if let Some(node) = rel_node(path, base)? {
            out.sources.push(CreateSource {
                path: path.to_path_buf(),
                node,
            });
        }
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(path)?
            .collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            collect_one(&e.path(), base, out)?;
        }
    } else {
        let node = rel_node(path, base)?.ok_or(ZipnestError::Security(
            archive_security::SecurityViolation::Empty,
        ))?;
        out.sources.push(CreateSource {
            path: path.to_path_buf(),
            node,
        });
    }
    Ok(())
}

/// Handler CLSID for the single-container formats written in one pass.
fn clsid_for(format: CreateFormat) -> Option<Guid> {
    match format {
        CreateFormat::Zip => Some(CLSID_FORMAT_ZIP),
        CreateFormat::SevenZ => Some(CLSID_FORMAT_7Z),
        CreateFormat::Tar => Some(CLSID_FORMAT_TAR),
        CreateFormat::TarGz | CreateFormat::TarBz2 | CreateFormat::TarXz => None,
    }
}

/// Wrapper handler CLSID for the compressed TAR variants.
fn wrapper_clsid(format: CreateFormat) -> Option<Guid> {
    match format {
        CreateFormat::TarGz => Some(CLSID_FORMAT_GZIP),
        CreateFormat::TarBz2 => Some(CLSID_FORMAT_BZIP2),
        CreateFormat::TarXz => Some(CLSID_FORMAT_XZ),
        _ => None,
    }
}

/// Deletes a temporary file when dropped, so cleanup covers both the success
/// and the error path of a two-pass create.
struct TempCleanup(PathBuf);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A unique scratch path beside the destination with extension `ext`, placed
/// there because that directory is known writable (the destination lives in
/// it), which keeps the write on one volume and lets tests observe cleanup.
/// The `zipnest` marker is what those tests look for.
fn temp_scratch_path(dest: &Path, ext: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    dest.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".{name}.zipnest-{}-{nanos}-{seq}.{ext}",
            std::process::id()
        ))
}

/// A unique scratch path for the intermediate `.tar`.
fn temp_tar_path(dest: &Path) -> PathBuf {
    temp_scratch_path(dest, "tar")
}

/// The path to hand to the OS, prefixing long absolute paths with the verbatim
/// `\\?\` marker so Windows doesn't reject them past MAX_PATH (os error
/// 123). Mirrors the extract-side helper (extract.rs) so the temp stream and
/// the final rename both survive a deep destination directory.
#[cfg(windows)]
fn os_path(p: &Path) -> PathBuf {
    const LIMIT: usize = 240;
    let s = p.as_os_str().to_string_lossy();
    if s.len() < LIMIT || !p.is_absolute() || s.starts_with(r"\\?\") {
        return p.to_path_buf();
    }
    let mut out = String::with_capacity(s.len() + 8);
    match s.strip_prefix(r"\\") {
        // A UNC path needs the `UNC\` form, not `\\?\` + `\\`.
        Some(rest) => {
            out.push_str(r"\\?\UNC\\");
            out.push_str(rest);
        }
        None => {
            out.push_str(r"\\?\\");
            out.push_str(&s);
        }
    }
    PathBuf::from(out.replace('/', "\\"))
}

#[cfg(not(windows))]
fn os_path(p: &Path) -> PathBuf {
    p.to_path_buf()
}

/// A unique temp sibling for an atomic single-file create: same directory as
/// `dest` so the final rename stays on one volume. The base name is trimmed
/// when the suffix would push the whole component past 255 bytes (a name may
/// itself be 255 bytes long). The `.zipnest-part-` marker is what the tests
/// look for to assert no leftover temp survives any path.
fn temp_part_path(dest: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    const MAX_COMPONENT: usize = 255;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let suffix = format!(
        ".zipnest-part-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut base = name;
    if base.len() + suffix.len() > MAX_COMPONENT {
        let mut cut = MAX_COMPONENT.saturating_sub(suffix.len());
        while cut > 0 && !base.is_char_boundary(cut) {
            cut -= 1;
        }
        base.truncate(cut);
    }
    let mut tmp = std::ffi::OsString::from(base);
    tmp.push(suffix);
    dest.with_file_name(tmp)
}

/// Inner node name for a compressed TAR: the destination filename with the
/// compression suffix removed (`out.tar.gz` -> `out.tar`). This is the name the
/// single compressed stream carries, so clients show `out.tar` rather than the
/// scratch filename.
fn inner_tar_node(dest: &Path) -> Result<String, ZipnestError> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().replace('\\', "/"))
        .ok_or(ZipnestError::Security(
            archive_security::SecurityViolation::Empty,
        ))?;
    let stripped = name
        .strip_suffix(".gz")
        .or_else(|| name.strip_suffix(".bz2"))
        .or_else(|| name.strip_suffix(".xz"))
        .unwrap_or(&name);
    Ok(archive_security::sanitize_entry_path(stripped)?)
}

/// File name of the vendored SFX stub for `kind`.
fn sfx_stub_name(kind: SfxKind) -> &'static str {
    match kind {
        SfxKind::Gui => "7z.sfx",
        SfxKind::Console => "7zCon.sfx",
    }
}

/// Locate the SFX stub for `kind`.
///
/// Search order mirrors [`crate::dll::load`]: an explicit override (dev/tests),
/// the deployed `<exe>/engines/sfx/` layout, then the vendored copy under
/// `vendor/7zip-sfx/` (the crate manifest dir at test time, or the workspace
/// root two levels up).
fn find_sfx_stub(kind: SfxKind) -> Result<PathBuf, ZipnestError> {
    let name = sfx_stub_name(kind);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("ZIPNEST_SFX_DIR") {
        if !dir.is_empty() {
            candidates.push(PathBuf::from(dir).join(name));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("engines").join("sfx").join(name));
        }
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("vendor").join("7zip-sfx").join(name));
    candidates.push(
        manifest
            .join("..")
            .join("..")
            .join("vendor")
            .join("7zip-sfx")
            .join(name),
    );
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or(ZipnestError::Engine(E_FAIL))
}

/// Concatenate `stub` then `archive` into `dest`, returning the byte length.
///
/// 7-Zip locates the archive by scanning from the tail, so a plain
/// `stub + 7z` byte stream is a valid SFX (the spike and `7z a -sfx` agree on
/// this layout).
///
/// 先写到 `dest` 旁边的临时文件，整段写完才 rename 覆盖 `dest` —— 与其它
/// 格式的 `write_archive(.., atomic_dest = true)` 同一套路。之前这里直接
/// `File::create(dest)` 截断，失败时再 `remove_file(dest)`：用户在覆盖确认里
/// 答应的是「替换」，拿到的却是「原来的没了、新的也没有」。现在失败只删临时
/// 文件，`dest` 上的旧内容原封不动。
fn concat_files(stub: &Path, archive: &Path, dest: &Path) -> Result<u64, ZipnestError> {
    use std::io::{Read, Write};
    let tmp = temp_part_path(dest);
    let guard = TempCleanup(tmp.clone());
    let copied = (|| -> std::io::Result<u64> {
        // 深层目录用 verbatim 前缀，否则超过 MAX_PATH 时这里先失败（os error 123）。
        let mut out = std::fs::File::create(os_path(&tmp))?;
        let mut buf = vec![0u8; 64 * 1024];
        for part in [stub, archive] {
            let mut f = std::fs::File::open(os_path(part))?;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
            }
        }
        out.flush()?;
        Ok(std::fs::metadata(os_path(&tmp))?.len())
    })()
    .and_then(|len| {
        // rename 成功即已提交：TempCleanup 随后的 remove_file 找不到临时文件，
        // 会静默失败（它是 drop 里的 best-effort），旧的 `dest` 则被替换。
        std::fs::rename(os_path(&tmp), os_path(dest)).map(|()| len)
    });
    drop(guard);
    copied.map_err(ZipnestError::Io)
}
/// Stat every source and build the item table; also returns the pre-scanned
/// total of file bytes.
fn stat_sources(sources: &[CreateSource]) -> Result<(Vec<SourceItem>, u64), ZipnestError> {
    let mut items = Vec::with_capacity(sources.len());
    let mut total_bytes = 0u64;
    for s in sources {
        let meta = std::fs::metadata(&s.path)?;
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        if !is_dir {
            total_bytes += size;
        }
        items.push(SourceItem {
            path: s.path.clone(),
            node: s.node.clone(),
            is_dir,
            size,
            mtime: meta.modified().ok(),
        });
    }
    Ok((items, total_bytes))
}

/// Best-effort archive-level properties via `ISetProperties`. Handlers that
/// don't implement the interface are simply left at their defaults.
unsafe fn apply_properties(raw: *mut c_void, opts: &CreateOptions) {
    let mut sp: *mut c_void = std::ptr::null_mut();
    let oav = &**(raw as *const *const OutArchiveVt);
    if (oav.query_interface)(raw, &IID_ISET_PROPERTIES, &mut sp) != S_OK || sp.is_null() {
        return;
    }
    // Wide names must outlive the call; keep the backing Vec<u16> alive.
    let mut names_keep: Vec<Vec<u16>> = Vec::new();
    let mut names: Vec<*const u16> = Vec::new();
    let mut values: Vec<PropVariant> = Vec::new();

    fn add(
        name: &str,
        pv: PropVariant,
        keep: &mut Vec<Vec<u16>>,
        names: &mut Vec<*const u16>,
        values: &mut Vec<PropVariant>,
    ) {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        keep.push(w);
        names.push(keep.last().unwrap().as_ptr());
        values.push(pv);
    }

    add(
        "x",
        PropVariant::from_u32(opts.level.engine_x()),
        &mut names_keep,
        &mut names,
        &mut values,
    );
    // Multi-threaded compression, as 7-Zip's own `-mmt=on` default. This is
    // what makes 7z/LZMA2 use every core; encoders that are single-threaded by
    // nature (ZIP's Deflate) simply ignore the property, and the resulting
    // archive stays an ordinary lossless one in both cases.
    add(
        "mt",
        PropVariant::from_bstr("on"),
        &mut names_keep,
        &mut names,
        &mut values,
    );
    if let Some(m) = opts.method.engine_name() {
        add(
            "m",
            PropVariant::from_bstr(m),
            &mut names_keep,
            &mut names,
            &mut values,
        );
    }
    if opts.password.is_some() && opts.format == CreateFormat::Zip {
        // ZipHandlerOut picks AES only when the `em` property says so; with a
        // password but no `em` the default is legacy ZipCrypto. "AES256" maps
        // to AES-256 key mode 3. (7z selects AES-256 automatically once the
        // password callback reports a password.)
        add(
            "em",
            PropVariant::from_bstr("AES256"),
            &mut names_keep,
            &mut names,
            &mut values,
        );
    }
    if opts.encrypt_names && opts.format == CreateFormat::SevenZ {
        // `he` (encrypt headers) seals the 7z index and file names as well as
        // the payload, so the archive cannot even be listed without the
        // password. It is a string property ("on"/"off"), not a boolean. ZIP
        // has no equivalent header encryption, so `encrypt_names` is ignored
        // there (the UI greys it out).
        add(
            "he",
            PropVariant::from_bstr("on"),
            &mut names_keep,
            &mut names,
            &mut values,
        );
    }

    let sp_vt = &**(sp as *const *const SetPropertiesVt);
    (sp_vt.set_properties)(sp, names.as_ptr(), values.as_ptr(), names.len() as u32);
    (sp_vt.release)(sp);
    for v in values.iter_mut() {
        v.clear(); // release the BSTR payloads
    }
}

/// Create an archive at `dest` from `sources`, reporting progress through
/// `progress` (return `false` to cancel).
///
/// `opts` carries the full creation surface; the compression level and method
/// are honored via `ISetProperties`. A password encrypts the payload: ZIP via
/// AES-256 (`em=AES256` plus the `ICryptoGetTextPassword2` callback), 7Z via
/// its default AES-256. Plain TAR writes in one pass; the compressed TAR
/// variants write a temporary `.tar` and wrap it. A password on the TAR family
/// is rejected with [`ZipnestError::PasswordUnsupported`]. With
/// `opts.volume_bytes`, ZIP/7z are split into `<dest>.001`, `.002`, ... (the
/// TAR family and SFX reject volumes with `E_NOTIMPL`). `opts.sfx` builds a
/// runnable `.exe` by prepending the matching stub to a 7z archive; it is
/// 7z-only.
pub fn create_archive(
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
    // SFX appends a 7z payload to an executable stub, so it is 7z-only and
    // cannot be split (the stub reads one contiguous stream). Reject the
    // impossible combinations before writing anything.
    if opts.sfx.is_some()
        && (opts.format != CreateFormat::SevenZ || opts.volume_bytes.is_some())
    {
        return Err(ZipnestError::Engine(E_NOTIMPL));
    }
    if let Some(limit) = opts.volume_bytes {
        // Only the single-container formats can be split; the TAR family and
        // SFX are out of scope. A zero volume size would divide the virtual
        // stream by zero, so it is rejected rather than silently ignored.
        if !matches!(opts.format, CreateFormat::Zip | CreateFormat::SevenZ) {
            return Err(ZipnestError::Engine(E_NOTIMPL));
        }
        if limit == 0 {
            return Err(ZipnestError::Engine(E_INVALIDARG));
        }
    }
    // The TAR family has no encryption; reject a password up front rather than
    // hand back an unencrypted archive the caller believes is protected.
    if opts.password.is_some()
        && matches!(
            opts.format,
            CreateFormat::Tar | CreateFormat::TarGz | CreateFormat::TarBz2 | CreateFormat::TarXz
        )
    {
        return Err(ZipnestError::PasswordUnsupported);
    }
    // 7z header encryption needs a key; without one the `he` property would be
    // a no-op at best and an engine failure at worst. Reject the impossible
    // combination instead of writing a plaintext index the caller thinks is
    // sealed. `PasswordRequired` is the closest existing i18n key.
    if opts.encrypt_names && opts.format == CreateFormat::SevenZ && opts.password.is_none() {
        return Err(ZipnestError::PasswordRequired);
    }
    if matches!(
        opts.format,
        CreateFormat::TarGz | CreateFormat::TarBz2 | CreateFormat::TarXz
    ) {
        return create_compressed_tar(sources, dest, opts, progress);
    }
    if let Some(kind) = opts.sfx {
        return create_sfx(kind, sources, dest, opts, progress);
    }
    let clsid = clsid_for(opts.format).ok_or(ZipnestError::Engine(E_NOTIMPL))?;
    write_archive(clsid, sources, dest, opts, progress, true)
}

/// Create a self-extracting `.exe`: build a normal 7z into a scratch file, then
/// prepend the stub for `kind` and write the concatenation to `dest`.
///
/// `rsfx` is deliberately not used: the SDK note (see
/// `vendor/7zip-sdk/Archive/IArchive.h`) says it makes a handler copy a stub it
/// already holds from an *opened* SFX. A fresh create has no such stub and
/// there is no API to hand one to a new handler, so manual concatenation — the
/// same layout `7z a -sfx` produces — is the correct mechanism. The scratch
/// archive is removed on both the success and the error path.
fn create_sfx(
    kind: SfxKind,
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
    let stub = find_sfx_stub(kind)?;
    let archive = temp_scratch_path(dest, "7z");
    let _cleanup = TempCleanup(archive.clone());

    // Build a plain .7z first; clearing `sfx` keeps this from recurring.
    let mut inner = opts.clone();
    inner.sfx = None;
    // Inner scratch write: `archive` is already a temp guarded by `_cleanup`,
    // so no second temp/rename layer is needed here.
    let stats = write_archive(CLSID_FORMAT_7Z, sources, &archive, &inner, progress, false)?;

    let bytes_out = concat_files(&stub, &archive, dest)?;
    Ok(CreateStats {
        files: stats.files,
        bytes_in: stats.bytes_in,
        bytes_out,
        kept_backup_files: stats.kept_backup_files,
    })
}

/// Two-pass create for `TarGz`/`TarBz2`/`TarXz`: build a temporary `.tar`, then
/// wrap it with the matching compressor. The temp file is removed on both the
/// success and the error path via [`TempCleanup`].
fn create_compressed_tar(
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
) -> Result<CreateStats, ZipnestError> {
    let wrapper = wrapper_clsid(opts.format).ok_or(ZipnestError::Engine(E_NOTIMPL))?;
    let tar_path = temp_tar_path(dest);
    let _cleanup = TempCleanup(tar_path.clone());

    // Inner scratch write: `tar_path` is already a temp guarded by `_cleanup`.
    let inner_stats = write_archive(CLSID_FORMAT_TAR, sources, &tar_path, opts, progress, false)?;

    let inner = [CreateSource {
        path: tar_path,
        node: inner_tar_node(dest)?,
    }];
    // The compressors ignore `m`/solid but honor `x`; dropping `m` avoids an
    // accidental "Copy disables compression" interpretation.
    let mut wrapper_opts = opts.clone();
    wrapper_opts.method = CompressionMethod::Auto;
    // Outer pass writes the final destination, so it gets the atomic temp+rename.
    let outer_stats = write_archive(wrapper, &inner, dest, &wrapper_opts, progress, true)?;

    Ok(CreateStats {
        files: inner_stats.files,
        bytes_in: inner_stats.bytes_in,
        bytes_out: outer_stats.bytes_out,
        kept_backup_files: outer_stats.kept_backup_files,
    })
}

/// The output stream handed to `UpdateItems`, tagged with its concrete kind.
///
/// `FileOutStream` and `VolumeOutStream` are distinct COM objects with different
/// vtables; releasing one through the other's `release` is type-confusion UB
/// (`Box::from_raw` on the wrong layout). Carrying the kind here means every
/// release/finalize site dispatches correctly instead of guessing from the
/// options.
enum OutTarget {
    File(*mut c_void),
    Volumes(*mut c_void),
}

impl OutTarget {
    fn as_ptr(&self) -> *mut c_void {
        match self {
            OutTarget::File(p) | OutTarget::Volumes(p) => *p,
        }
    }

    /// Release exactly the object this variant was created from.
    ///
    /// # Safety
    /// `self` must wrap a live pointer returned by the matching
    /// `outstream::new` / `outstream::new_volumes`.
    unsafe fn release(self) {
        match self {
            OutTarget::File(p) => outstream::release_void(p),
            OutTarget::Volumes(p) => outstream::release_void_volumes(p),
        }
    }
}

/// 暂存卷的清理守卫：drop 时删掉暂存前缀下**还没搬走**的 `.<NNN>` 卷。
///
/// 只认我们自己刚写的暂存前缀（`<dest>.zipnest-part-<pid>-<seq>`），所以它不会
/// 碰到任何用户可见的文件。成功的替换做完后暂存卷已经全部改名搬走，这里再扫
/// 一遍什么也找不到，因此不需要「成功就解除守卫」这种分支。
struct StagedVolumeCleanup {
    prefix: PathBuf,
}

impl Drop for StagedVolumeCleanup {
    fn drop(&mut self) {
        for path in outstream::written_volume_series(&self.prefix) {
            let _ = std::fs::remove_file(os_path(&path));
        }
    }
}

/// 一次分卷创建过程中需要跨 `UpdateItems` 存活的东西：互斥量、暂存前缀、
/// 暂存卷的清理守卫。三者的生命周期都必须覆盖到「替换完成」为止。
struct VolumeRun {
    /// 进程间互斥量。持有到替换结束，保证同一 `dest` 只有一个任务在动文件。
    _lock: outstream::VolumeCreateLock,
    /// 暂存前缀（`<dest>.zipnest-part-<pid>-<seq>`，不带结尾的点）。
    staging: PathBuf,
    _cleanup: StagedVolumeCleanup,
}

/// 备份卷名里的标签，保证同一 dest 的连续两次覆盖不会互相踩。
///
/// 用 `pid + 单调时间戳`：跨进程靠 pid、同进程靠时间戳。真正防撞的是
/// **同一时刻只有一个任务持有该 dest 的互斥量**，这里只要保证「上一次的备份
/// 名」不会和这一次的重合即可（重合的唯一后果是覆盖掉上一个残留备份，
/// 那本来也是我们自己的垃圾）。
fn backup_tag() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        nanos,
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `<dest>` 的第 `index`（从 1 起）个卷的最终路径：`<dest>.NNN`。
fn final_volume_path(dest: &Path, index: usize) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(format!(".{index:03}"));
    PathBuf::from(name)
}

/// 旧卷 `index`（从 1 起）的备份路径：`<dest>.old-<tag>.NNN`。
///
/// 文件名组件上限是 255 字节，所以 `dest` 的名字很长时先把基名截短再拼后缀；
/// 编号原样保留，这样回滚时能与原路径一一对应（原路径本身由调用方记着，
/// 不靠这条命名反推）。
fn backup_volume_path(dest: &Path, tag: &str, index: usize) -> PathBuf {
    const MAX_COMPONENT: usize = 255;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let suffix = format!(".old-{tag}.{index:03}");
    let mut base = name;
    if base.len() + suffix.len() > MAX_COMPONENT {
        let mut cut = MAX_COMPONENT.saturating_sub(suffix.len());
        while cut > 0 && !base.is_char_boundary(cut) {
            cut -= 1;
        }
        base.truncate(cut);
    }
    let mut out = std::ffi::OsString::from(base);
    out.push(&suffix);
    dest.with_file_name(out)
}

/// 把一组「备份名 → 原名」的改名反向执行一次，返回失败的描述。
///
/// 反向改名只做 best-effort 收集，不提前返回：能救回多少是多少，剩下的必须
/// 原样写进错误信息交出去。
fn restore_backups(moved: &[(PathBuf, PathBuf)]) -> Vec<String> {
    let mut failures = Vec::new();
    // 倒序回滚：与改名时的顺序相反，读起来和现场一致。
    for (original, backup) in moved.iter().rev() {
        if let Err(e) = std::fs::rename(os_path(backup), os_path(original)) {
            failures.push(format!(
                "{} -> {}: {e}",
                backup.display(),
                original.display()
            ));
        }
    }
    failures
}

/// 替换失败时统一拼错误：失败在哪一步、原始 io 错误、以及回滚的结果。
///
/// 「回滚本身也失败」时错误信息必须点名**哪些文件停在了什么路径**，否则用户
/// 手上是一堆没人认识的 `.old-<tag>.NNN`。
fn replacement_error(stage: &str, cause: std::io::Error, rollback: &[String]) -> ZipnestError {
    let mut msg = format!("volume replacement failed while {stage}: {cause}");
    if rollback.is_empty() {
        msg.push_str("; the previous volume set was restored unchanged");
    } else {
        msg.push_str("; rollback could not finish, these files were left where they are: ");
        msg.push_str(&rollback.join("; "));
    }
    ZipnestError::Io(std::io::Error::other(msg))
}

/// 把暂存卷整体替换到 `<dest>.NNN`，返回「删不掉的备份」清单。
///
/// 协议（每一步失败都回滚，任何失败之后旧归档都必须仍然可用）：
/// 1. 列出暂存卷（升序）与旧序列（`existing_volume_series`：只认 `<dest>.NNN`
///    的连续段，别的程序的文件一个都不碰）；
/// 2. 把旧卷逐个改名为 `<dest>.old-<tag>.NNN` 并记下 (原名, 备份名)，失败就
///    反向改回；
/// 3. 把暂存卷按编号升序改名为 `<dest>.NNN`，失败就删掉已就位的新卷、把备份
///    改回原名；
/// 4. 删除备份，**best-effort**：删不掉留在原地并如实返回，不因此判失败。
///
/// 全程只做改名与删除，不复制字节，所以替换本身与归档大小无关。
fn commit_volume_series(dest: &Path, staging: &Path) -> Result<Vec<PathBuf>, ZipnestError> {
    let staged = outstream::written_volume_series(staging);
    if staged.is_empty() {
        return Err(ZipnestError::Io(std::io::Error::other(
            "volume replacement failed: the staged volume set is empty",
        )));
    }
    let old = outstream::existing_volume_series(dest).map_err(ZipnestError::Io)?;
    let tag = backup_tag();

    // 第 2 步：旧卷先让位到备份名。全程只改名，旧内容一个字节都没动。
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(old.len());
    for (i, original) in old.iter().enumerate() {
        let backup = backup_volume_path(dest, &tag, i + 1);
        if let Err(e) = std::fs::rename(os_path(original), os_path(&backup)) {
            let rollback = restore_backups(&moved);
            return Err(replacement_error(
                &format!("backing up {}", original.display()),
                e,
                &rollback,
            ));
        }
        moved.push((original.clone(), backup));
    }

    // 第 3 步：暂存卷就位。按编号升序，失败时把已经就位的这些删掉，再把备份
    // 改回原名 —— 目录回到「旧归档完好」的状态。
    let mut placed: Vec<PathBuf> = Vec::with_capacity(staged.len());
    for (i, from) in staged.iter().enumerate() {
        let to = final_volume_path(dest, i + 1);
        if let Err(e) = std::fs::rename(os_path(from), os_path(&to)) {
            let mut rollback = Vec::new();
            for path in &placed {
                if let Err(e) = std::fs::remove_file(os_path(path)) {
                    rollback.push(format!("{}: could not remove the new volume: {e}", path.display()));
                }
            }
            rollback.extend(restore_backups(&moved));
            return Err(replacement_error(
                &format!("installing {}", to.display()),
                e,
                &rollback,
            ));
        }
        placed.push(to);
    }

    // 第 4 步：备份删除是收尾工作，删不掉不算失败。
    let mut leftover = Vec::new();
    for (_, backup) in &moved {
        match std::fs::remove_file(os_path(backup)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => leftover.push(backup.clone()),
        }
    }
    Ok(leftover)
}

/// 分卷锁的错误映射：`WouldBlock`（另一个任务持有）单独成
/// [`ZipnestError::VolumeBusy`]，其余 io 错误照常是 `error.io`。
///
/// 抽成函数是为了能被单测直接打到 —— 真要端到端复现「另一个任务正持有锁」，
/// 得让 `create_archive` 原地等满 [`outstream::VOLUME_CREATE_LOCK_TIMEOUT_MS`]
/// 十秒，不值得让每次跑测试都付这个时间。
fn volume_lock_error(e: std::io::Error) -> ZipnestError {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        ZipnestError::VolumeBusy
    } else {
        ZipnestError::Io(e)
    }
}

/// Run one `IOutArchive::UpdateItems` pass for `clsid`, writing all `sources`
/// into the archive at `dest`.
#[allow(clippy::arc_with_non_send_sync)] // ProgressCell holds a stack-bound closure cell
fn write_archive(
    clsid: Guid,
    sources: &[CreateSource],
    dest: &Path,
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&CreateProgress) -> bool,
    atomic_dest: bool,
) -> Result<CreateStats, ZipnestError> {
    let (items, total_bytes) = stat_sources(sources)?;
    let files = items.iter().filter(|i| !i.is_dir).count() as u32;
    let num_items = items.len() as u32;

    let dll = dll::load()?;
    // Open the output stream first: if this fails, no handler ref exists yet.
    // With a volume size, the stream spans `<staging>.001`, `.002`, ... instead
    // of a single `<dest>`; the handler is unaware and simply writes bytes
    // (7-Zip's own client does exactly this, via its `CMultiOutStream`).
    // An atomic single-file create writes to a temp sibling and renames on
    // success; the guard removes the temp on every path.
    let mut atomic_tmp: Option<(PathBuf, TempCleanup)> = None;
    // 分卷的现场（锁 + 暂存前缀 + 清理守卫）。只有分卷分支会填它。
    let mut volume_run: Option<VolumeRun> = None;
    let out = match opts.volume_bytes {
        Some(limit) => {
            // 并发保护：同一个 `dest` 只允许一个任务做分卷替换。Windows 允许
            // 删除别的进程正打开着的文件，所以「先删旧卷再写」会让两个窗口
            // 互删对方正在写的 `.001`，两边都报成功、磁盘上只剩孤卷。拿不到锁
            // 就如实报 `error.volume_busy`，不去抢文件。
            let lock = outstream::acquire_volume_lock(dest).map_err(volume_lock_error)?;
            // 先写暂存前缀：全部卷 finalize 成功之前，用户可见的 `<dest>.NNN`
            // 一个都不动。取消、源文件读不到、磁盘满都只会留下暂存卷，随后由
            // 守卫删掉；旧归档原封不动。
            let staging = outstream::staging_volume_prefix(dest);
            let cleanup = StagedVolumeCleanup {
                prefix: staging.clone(),
            };
            let stream = outstream::new_volumes(&staging, limit)?;
            volume_run = Some(VolumeRun {
                _lock: lock,
                staging,
                _cleanup: cleanup,
            });
            OutTarget::Volumes(stream)
        }
        None if atomic_dest => {
            let tmp = temp_part_path(dest);
            let stream = outstream::new(&tmp)?;
            atomic_tmp = Some((tmp.clone(), TempCleanup(tmp)));
            OutTarget::File(stream)
        }
        None => OutTarget::File(outstream::new(dest)?),
    };

    let mut raw: *mut c_void = std::ptr::null_mut();
    let hr = unsafe { dll.create_object(&clsid, &IID_IOUT_ARCHIVE, &mut raw) };
    if hr != S_OK || raw.is_null() {
        // Release through the same tag used to create it, not a guessed vtable.
        unsafe { out.release() };
        return Err(crate::error::map_hresult(hr));
    }

    // Apply archive-level properties (compression level/method) before items.
    unsafe { apply_properties(raw, opts) };

    let state = Arc::new(UpdateState {
        done_bytes: AtomicU64::new(0),
        total_bytes: AtomicU64::new(total_bytes),
        cancelled: AtomicU8::new(0),
        last_path: Mutex::new(String::new()),
        progress: Mutex::new(outcallback::progress_cell(progress)),
        io_error: Mutex::new(None),
    });
    let crypto = callbacks::crypto_new(opts.password.clone());
    let cb = outcallback::new(items, Arc::clone(&state), crypto as *mut c_void);

    let hr = unsafe {
        let vt = &**(raw as *const *const OutArchiveVt);
        (vt.update_items)(raw, out.as_ptr(), num_items, cb)
    };

    let cancelled = state.cancelled.load(Ordering::SeqCst) == 1;
    let io_error = state.io_error.lock().ok().and_then(|mut s| s.take());
    let update_ok = hr == S_OK && !cancelled && io_error.is_none();

    // Drop our references: the callback, the output stream, and the handler.
    // A multi-volume stream is finalized only on success (truncate each volume
    // to its logical size and keep the files); on any failure a plain release
    // makes its destructor delete the partial volumes.
    let mut volume_bytes = 0u64;
    let mut finalize_failed = false;
    unsafe {
        match out {
            OutTarget::Volumes(p) => {
                if update_ok {
                    match outstream::finalize_volumes(p) {
                        Some(n) => volume_bytes = n,
                        None => finalize_failed = true,
                    }
                }
                outstream::release_void_volumes(p);
            }
            OutTarget::File(p) => outstream::release_void(p),
        }
        outcallback::release_void(cb);
        let vt = &**(raw as *const *const OutArchiveVt);
        (vt.release)(raw);
        // Free the boxed closure cell; no callbacks can fire after UpdateItems.
        let cell = state
            .progress
            .lock()
            .map(|c| c.cell)
            .unwrap_or(std::ptr::null_mut());
        if !cell.is_null() {
            drop(Box::from_raw(
                cell as *mut &mut dyn FnMut(&CreateProgress) -> bool,
            ));
        }
    }

    // Post-check order: local flags win over the handler's HRESULT (a cancel
    // we raised must report its real cause).
    if cancelled {
        return Err(ZipnestError::Cancelled);
    }
    if let Some(e) = io_error {
        return Err(ZipnestError::Io(e));
    }
    if finalize_failed {
        return Err(ZipnestError::Engine(E_FAIL));
    }
    if hr != S_OK {
        return Err(crate::error::map_hresult(hr));
    }

    let mut kept_backups = 0u32;
    let bytes_out = if let Some(run) = volume_run.take() {
        // 到这里暂存卷已全部 finalize 成功，才动用户可见的 `<dest>.NNN`。
        // 失败时 `commit_volume_series` 已把旧卷改回原名（回滚细节见它的文档），
        // 退出本函数时 `run` 的清理守卫再把剩下的暂存卷删掉。
        let leftover = commit_volume_series(dest, &run.staging)?;
        // 备份删不掉不算失败：旧内容仍然在磁盘上，只是多了几个 `.old-<tag>.NNN`。
        // 数字交回调用方，由它决定怎么告诉用户。
        kept_backups = leftover.len() as u32;
        volume_bytes
    } else if let Some((tmp, _guard)) = atomic_tmp.take() {
        // Commit the finished archive over `dest`. The TempCleanup guard is
        // dropped here on success too, but rename means its remove_file is a
        // no-op. A rename failure (target locked, disk full) surfaces as Io and
        // leaves the original `dest` untouched.
        std::fs::rename(os_path(&tmp), os_path(dest)).map_err(ZipnestError::Io)?;
        std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
    } else {
        std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
    };

    Ok(CreateStats {
        files,
        bytes_in: total_bytes,
        bytes_out,
        kept_backup_files: kept_backups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zn-create-unit-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 造一套「暂存卷 + 旧卷」的目录现场：旧卷 2 个、暂存卷 3 个，内容各自
    /// 可辨认，方便断言回滚之后谁是谁。
    fn seed(dir: &Path, old: &[&[u8]], staged: &[&[u8]]) -> (PathBuf, PathBuf) {
        let dest = dir.join("vol.7z");
        let staging = outstream::staging_volume_prefix(&dest);
        for (i, body) in old.iter().enumerate() {
            std::fs::write(final_volume_path(&dest, i + 1), body).unwrap();
        }
        for (i, body) in staged.iter().enumerate() {
            let mut name = staging.as_os_str().to_os_string();
            name.push(format!(".{:03}", i + 1));
            std::fs::write(PathBuf::from(name), body).unwrap();
        }
        (dest, staging)
    }

    /// 正常的替换：新卷就位、旧卷变成备份后删掉，返回的残留清单为空。
    #[test]
    fn commit_replaces_the_old_series_and_deletes_backups() {
        let dir = temp_dir("commit-ok");
        let (dest, staging) = seed(&dir, &[b"old1", b"old2"], &[b"new1", b"new2", b"new3"]);
        let leftover = commit_volume_series(&dest, &staging).expect("replace must succeed");
        assert!(leftover.is_empty(), "backups must be gone: {leftover:?}");
        let vols = outstream::existing_volume_series(&dest).unwrap();
        assert_eq!(vols.len(), 3, "the new series has three volumes");
        for (i, body) in [&b"new1"[..], b"new2", b"new3"].iter().enumerate() {
            assert_eq!(&std::fs::read(final_volume_path(&dest, i + 1)).unwrap(), body);
        }
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".old-")), "no backup may survive: {names:?}");
        assert!(names.iter().all(|n| !n.contains("zipnest-part")), "no staging may survive: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回滚路径（人为让「新卷就位」这一步失败）：已让位的旧卷必须全部改回原名，
    /// 已就位的新卷必须删掉，目录回到替换之前的样子。
    ///
    /// 构造手法：在第二个暂存卷的**最终位置**上先建一个同名目录。旧序列扫描
    /// 遇到非普通文件就停，所以旧归档只有 `.001`；它照常让位到备份，`.001`
    /// 的新卷也能就位，轮到 `.002` 改名到那个目录上时必定失败 —— 正好检验
    /// 「删掉已就位的新卷 + 把备份改回原名」这条回滚分支。
    #[test]
    fn a_failed_install_rolls_the_old_series_back() {
        let dir = temp_dir("commit-rollback");
        let (dest, staging) = seed(&dir, &[b"old1"], &[b"new1", b"new2", b"new3"]);
        std::fs::create_dir_all(final_volume_path(&dest, 2)).unwrap();
        std::fs::write(final_volume_path(&dest, 2).join("blocker"), b"x").unwrap();

        let err = commit_volume_series(&dest, &staging)
            .expect_err("installing the second volume must fail");
        assert_eq!(err.error_key(), "error.io", "got {err:?}");
        // 错误信息必须说清回滚结果，不能只说「失败了」。`Display` 对 `Io` 只
        // 打印 error key，所以这里读的是 io 错误自带的说明。
        let ZipnestError::Io(io) = &err else { panic!("expected an io error, got {err:?}") };
        let text = io.to_string();
        assert!(text.contains("volume replacement failed"), "got {text}");
        assert!(text.contains("restored unchanged"), "the message must state the rollback outcome, got {text}");

        // 旧卷回到原名，内容逐字节未变；挡住位置的那个目录原封不动。
        assert_eq!(std::fs::read(final_volume_path(&dest, 1)).unwrap(), b"old1",
            "the old volume must be back under its own name");
        assert!(final_volume_path(&dest, 2).is_dir(), "the blocker directory is untouched");
        // 没有留下任何备份，也没有把新卷留在目标序列里。
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".old-")), "rollback must leave no backup: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 锁错误映射：`WouldBlock` 变成 `error.volume_busy`，别的 io 错误仍是
    /// `error.io`。并发冲突在 UI 上必须指向「另一个任务正在写这个位置」，
    /// 而不是让用户去查权限或磁盘。
    #[test]
    fn lock_contention_maps_to_volume_busy() {
        let busy = std::io::Error::new(std::io::ErrorKind::WouldBlock, "held");
        assert_eq!(volume_lock_error(busy).error_key(), "error.volume_busy");
        let other = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        assert_eq!(volume_lock_error(other).error_key(), "error.io");
    }
}

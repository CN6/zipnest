//! Which containers the app recognises, and how each one is opened.
//!
//! Two different questions live here, and keeping them apart is the point:
//!
//! * **Can this file be opened?** — answered from its *bytes*. `candidates_for`
//!   tries the handler a file name suggests, then falls back to asking every
//!   handler in turn (`Archive::open` does the trying). A `.zst` named
//!   `.zip`, or a `.docx`, still opens as what it is.
//! * **Should this extension be claimed by default?** — answered from a fixed
//!   table of extensions. Formats whose double-click means something else
//!   (`.msi` installs, `.chm` opens help, `.cab` is a driver package) are
//!   listed as `OpenWith` only: they appear in ZipNest's 打开方式 list but are
//!   never taken over as the default handler.

use crate::com::Guid;
use crate::com::*;

/// How an extension may be associated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// A compressed archive with no other meaning on Windows: claiming the
    /// default is what a user expects from an archiver.
    Default,
    /// A container that Windows (or its user) gives another job: only ever
    /// listed under "open with", never made the default.
    OpenWith,
}

/// One row of the association table.
#[derive(Debug, Clone, Copy)]
pub struct FormatRow {
    pub ext: &'static str,
    pub claim: Claim,
    /// Short label for the settings page.
    pub label: &'static str,
}

/// Every extension the app can open, in the order the settings page lists them.
///
/// `.zst`/`.tzst` and `.lzma` are decode-only: 7-Zip cannot *create* zstd (the
/// engine has no zstd encoder), and the single-stream formats have no index to
/// append to. That is a property of the handlers, not a policy choice, so it is
/// noted here rather than enforced with a flag.
pub const FORMATS: &[FormatRow] = &[
    FormatRow { ext: "zip", claim: Claim::Default, label: "ZIP" },
    FormatRow { ext: "7z", claim: Claim::Default, label: "7z" },
    FormatRow { ext: "rar", claim: Claim::Default, label: "RAR" },
    FormatRow { ext: "tar", claim: Claim::Default, label: "TAR" },
    FormatRow { ext: "gz", claim: Claim::Default, label: "GZ" },
    FormatRow { ext: "tgz", claim: Claim::Default, label: "TGZ" },
    FormatRow { ext: "bz2", claim: Claim::Default, label: "BZ2" },
    FormatRow { ext: "xz", claim: Claim::Default, label: "XZ" },
    FormatRow { ext: "iso", claim: Claim::Default, label: "ISO" },
    // Decode-only, but still an archive and nothing else: claiming them is
    // what a user means by "make ZipNest my archiver".
    FormatRow { ext: "zst", claim: Claim::Default, label: "ZST" },
    FormatRow { ext: "tzst", claim: Claim::Default, label: "TZST" },
    FormatRow { ext: "lzma", claim: Claim::Default, label: "LZMA" },
    FormatRow { ext: "arj", claim: Claim::Default, label: "ARJ" },
    FormatRow { ext: "lzh", claim: Claim::Default, label: "LZH" },
    FormatRow { ext: "cab", claim: Claim::OpenWith, label: "CAB" },
    FormatRow { ext: "xar", claim: Claim::Default, label: "XAR" },
    FormatRow { ext: "cpio", claim: Claim::Default, label: "CPIO" },
    FormatRow { ext: "rpm", claim: Claim::Default, label: "RPM" },
    FormatRow { ext: "deb", claim: Claim::Default, label: "DEB" },
    FormatRow { ext: "msi", claim: Claim::OpenWith, label: "MSI" },
    FormatRow { ext: "wim", claim: Claim::Default, label: "WIM" },
    FormatRow { ext: "swm", claim: Claim::Default, label: "SWM" },
    FormatRow { ext: "dmg", claim: Claim::Default, label: "DMG" },
    FormatRow { ext: "chm", claim: Claim::OpenWith, label: "CHM" },
    FormatRow { ext: "squashfs", claim: Claim::Default, label: "SquashFS" },
    FormatRow { ext: "vhd", claim: Claim::Default, label: "VHD" },
    FormatRow { ext: "vhdx", claim: Claim::Default, label: "VHDX" },
    FormatRow { ext: "udf", claim: Claim::Default, label: "UDF" },
    FormatRow { ext: "apfs", claim: Claim::Default, label: "APFS" },
    FormatRow { ext: "ext", claim: Claim::Default, label: "EXT" },
    FormatRow { ext: "ext2", claim: Claim::Default, label: "EXT2" },
    FormatRow { ext: "ext3", claim: Claim::Default, label: "EXT3" },
    FormatRow { ext: "ext4", claim: Claim::Default, label: "EXT4" },
    FormatRow { ext: "vmdk", claim: Claim::Default, label: "VMDK" },
    FormatRow { ext: "qcow", claim: Claim::Default, label: "QCOW" },
    FormatRow { ext: "z", claim: Claim::Default, label: "Z" },
];

/// Every extension, in table order.
pub fn all_extensions() -> Vec<&'static str> {
    FORMATS.iter().map(|f| f.ext).collect()
}

/// The extensions a default claim may cover — everything except the formats
/// whose double-click means something else.
pub fn default_claim_extensions() -> Vec<&'static str> {
    FORMATS
        .iter()
        .filter(|f| f.claim == Claim::Default)
        .map(|f| f.ext)
        .collect()
}

/// Extensions that are "open with" only.
pub fn open_with_only() -> Vec<&'static str> {
    FORMATS
        .iter()
        .filter(|f| f.claim == Claim::OpenWith)
        .map(|f| f.ext)
        .collect()
}

/// Does this path *look like* something the app should try to open?
///
/// Used to decide what a drop does, before anything is decoded. It is a hint,
/// not a gate: a file whose extension is unknown is still attempted when the
/// caller wants to (the drag-and-drop handler tries the first unknown file and
/// lets the engine's signature check answer). `.001`-style volumes are covered
/// through the format they split from.
pub fn looks_like_archive(path: &str) -> bool {
    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if FORMATS.iter().any(|f| f.ext == ext) {
        return true;
    }
    // `.001`, `.0001`, … — 7-Zip's Split handler needs at least two digits.
    if ext.len() < 2 || !ext.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    p.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .and_then(|s| s.rsplit('.').next().map(str::to_string))
        .is_some_and(|inner| FORMATS.iter().any(|f| f.ext == inner))
}

/// Handler candidates for an extension, best first.
///
/// An empty list means "no extension claimed this, so let the caller try every
/// handler in turn" — `Archive::open` does exactly that for an empty candidate
/// list when the caller asks for it. The numbers come from
/// `vendor/7zip-sdk/Guid.txt`.
pub fn candidates_for(ext: &str) -> Vec<Guid> {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    match e.as_str() {
        "zip" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "jar" | "apk" | "epub"
        | "whl" | "xpi" => vec![CLSID_FORMAT_ZIP],
        "7z" => vec![CLSID_FORMAT_7Z],
        "rar" => vec![CLSID_FORMAT_RAR5, CLSID_FORMAT_RAR],
        "tar" => vec![CLSID_FORMAT_TAR],
        "gz" | "tgz" => vec![CLSID_FORMAT_GZIP],
        "bz2" | "tbz2" | "tbz" => vec![CLSID_FORMAT_BZIP2],
        "xz" | "txz" => vec![CLSID_FORMAT_XZ],
        "iso" => vec![CLSID_FORMAT_ISO],
        "zst" | "tzst" => vec![CLSID_FORMAT_ZSTD],
        "lzma" => vec![CLSID_FORMAT_LZMA],
        "arj" => vec![CLSID_FORMAT_ARJ],
        "lzh" | "lha" => vec![CLSID_FORMAT_LZH],
        "cab" => vec![CLSID_FORMAT_CAB],
        "xar" => vec![CLSID_FORMAT_XAR],
        "cpio" => vec![CLSID_FORMAT_CPIO],
        "rpm" => vec![CLSID_FORMAT_RPM],
        "deb" => vec![CLSID_FORMAT_DEB],
        "msi" => vec![CLSID_FORMAT_COMPOUND],
        "wim" | "swm" => vec![CLSID_FORMAT_WIM],
        "dmg" => vec![CLSID_FORMAT_DMG],
        "chm" => vec![CLSID_FORMAT_CHM],
        "squashfs" => vec![CLSID_FORMAT_SQUASHFS],
        "vhd" => vec![CLSID_FORMAT_VHD],
        "vhdx" => vec![CLSID_FORMAT_VHDX],
        "udf" => vec![CLSID_FORMAT_UDF],
        "apfs" => vec![CLSID_FORMAT_APFS],
        "ext" | "ext2" | "ext3" | "ext4" => vec![CLSID_FORMAT_EXT],
        "vmdk" => vec![CLSID_FORMAT_VMDK],
        "qcow" | "qcow2" => vec![CLSID_FORMAT_QCOW],
        "z" => vec![CLSID_FORMAT_Z],
        _ => Vec::new(),
    }
}

/// The handler CLSIDs worth trying for a file when the extension says nothing
/// (or lies). Ordered so the common formats are tried first.
///
/// The compound (`.msi`) handler goes last: it opens *any* OLE2 document, so
/// trying it early would make every doc/xls with an archive extension look
/// like an archive.
pub fn signature_probe_order() -> Vec<Guid> {
    vec![
        CLSID_FORMAT_ZIP,
        CLSID_FORMAT_7Z,
        CLSID_FORMAT_RAR5,
        CLSID_FORMAT_RAR,
        CLSID_FORMAT_TAR,
        CLSID_FORMAT_GZIP,
        CLSID_FORMAT_BZIP2,
        CLSID_FORMAT_XZ,
        CLSID_FORMAT_ZSTD,
        CLSID_FORMAT_ISO,
        CLSID_FORMAT_CAB,
        CLSID_FORMAT_ARJ,
        CLSID_FORMAT_LZH,
        CLSID_FORMAT_XAR,
        CLSID_FORMAT_CPIO,
        CLSID_FORMAT_RPM,
        CLSID_FORMAT_DEB,
        CLSID_FORMAT_WIM,
        CLSID_FORMAT_DMG,
        CLSID_FORMAT_CHM,
        CLSID_FORMAT_SQUASHFS,
        CLSID_FORMAT_VHD,
        CLSID_FORMAT_VHDX,
        CLSID_FORMAT_UDF,
        CLSID_FORMAT_APFS,
        CLSID_FORMAT_EXT,
        CLSID_FORMAT_LZMA,
        CLSID_FORMAT_Z,
        CLSID_FORMAT_COMPOUND,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formats whose double-click means something other than "unpack".
    #[test]
    fn msi_chm_cab_are_never_claimed_by_default() {
        for ext in ["msi", "chm", "cab"] {
            let row = FORMATS.iter().find(|f| f.ext == ext).unwrap();
            assert_eq!(row.claim, Claim::OpenWith, "{ext} must be open-with only");
            assert!(
                !default_claim_extensions().contains(&ext),
                "{ext} must not be in the default claim list"
            );
            assert!(open_with_only().contains(&ext));
            assert!(all_extensions().contains(&ext), "{ext} must still be openable");
        }
    }

    /// Every extension in the table is openable, and the two questions agree:
    /// Default ⊂ all, OpenWith ∩ Default = ∅, the union is everything.
    #[test]
    fn the_claim_table_is_partitioned() {
        let all = all_extensions();
        let def = default_claim_extensions();
        let open_only = open_with_only();
        assert_eq!(def.len() + open_only.len(), all.len());
        for e in &def {
            assert!(!open_only.contains(e), "{e} is in both lists");
            assert!(all.contains(e));
        }
        // No duplicates in the table.
        let mut seen = std::collections::HashSet::new();
        for e in &all {
            assert!(seen.insert(*e), "duplicate extension {e}");
        }
        // The nine the app shipped before are all still claimed by default:
        // an upgrade must not silently stop opening .zip.
        for e in ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"] {
            assert!(def.contains(&e), "{e} must stay a default claim");
        }
    }

    /// Candidates for unknown extensions are empty (so the signature probe
    /// decides), and every table row answers something.
    #[test]
    fn every_known_extension_has_a_handler_and_unknowns_do_not() {
        for e in all_extensions() {
            assert!(!candidates_for(e).is_empty(), "no handler for .{e}");
        }
        assert!(candidates_for("txt").is_empty());
        assert!(candidates_for("").is_empty());
        // A dotted spelling is the same extension.
        assert_eq!(candidates_for(".zip"), candidates_for("zip"));
        assert_eq!(candidates_for("ZIP"), candidates_for("zip"));
    }

    #[test]
    fn looks_like_archive_covers_the_table_and_volumes() {
        assert!(looks_like_archive("a.zip"));
        assert!(looks_like_archive("A.ZST"));
        assert!(looks_like_archive("backup.7z.001"));
        assert!(looks_like_archive("x.tar.zst.0001"));
        // A raw numbered file with an unknown base is not an archive.
        assert!(!looks_like_archive("photo.001"));
        assert!(!looks_like_archive("notes.txt"));
        assert!(!looks_like_archive("a.exe"));
    }
}
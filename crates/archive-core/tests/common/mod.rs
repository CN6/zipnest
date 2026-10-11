//! Shared fixture for the "nothing is ever lost" tests: a ZIP writer that puts
//! raw, hostile bytes in the central directory (the `zip` crate would reject or
//! rewrite them), plus small helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[derive(Default)]
pub struct Zip {
    local: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl Zip {
    pub fn add(&mut self, name: &[u8], body: &[u8]) {
        let offset = self.local.len() as u32;
        let crc = crc32(body);
        let len = body.len() as u32;
        let mut h = Vec::new();
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local header
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 name flag
        h.extend_from_slice(&0u16.to_le_bytes()); // store
        h.extend_from_slice(&0u16.to_le_bytes()); // mtime
        h.extend_from_slice(&0u16.to_le_bytes()); // mdate
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&len.to_le_bytes());
        h.extend_from_slice(&len.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // extra
        h.extend_from_slice(name);
        h.extend_from_slice(body);
        self.local.extend_from_slice(&h);

        let mut c = Vec::new();
        c.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central header
        c.extend_from_slice(&20u16.to_le_bytes()); // version made by
        c.extend_from_slice(&20u16.to_le_bytes()); // version needed
        c.extend_from_slice(&0x0800u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes()); // mtime
        c.extend_from_slice(&0u16.to_le_bytes()); // mdate
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&len.to_le_bytes());
        c.extend_from_slice(&len.to_le_bytes());
        c.extend_from_slice(&(name.len() as u16).to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes()); // extra
        c.extend_from_slice(&0u16.to_le_bytes()); // comment
        c.extend_from_slice(&0u16.to_le_bytes()); // disk
        c.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        c.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        c.extend_from_slice(&offset.to_le_bytes());
        c.extend_from_slice(name);
        self.central.extend_from_slice(&c);
        self.count += 1;
    }

    pub fn finish(self) -> Vec<u8> {
        let cd_offset = self.local.len() as u32;
        let cd_size = self.central.len() as u32;
        let mut out = self.local;
        out.extend_from_slice(&self.central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // EOCD
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }
}

pub fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Every file under `dir`, recursively, as `(relative path with '/', contents)`.
pub fn walk_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// One archived entry: its raw name and the bytes it holds.
pub type Case = (Vec<u8>, Vec<u8>);

/// The archive the tests work with: one entry per way an archive can carry a
/// name Windows refuses to store, plus a normal tree and one directory.
///
/// Returns `(zip path, file cases, directory entry names)`.
pub fn hostile_zip(name: &str) -> (PathBuf, Vec<Case>, Vec<Vec<u8>>) {
    let long = format!("{}.txt", "L".repeat(300));
    let cases: Vec<Case> = vec![
        (b"normal/ok.txt".to_vec(), b"plain".to_vec()),
        (b"deep/a/b/c/d/e/f/g.txt".to_vec(), b"nested".to_vec()),
        (b"CON".to_vec(), b"device-con".to_vec()),
        (b"dir/aux.txt".to_vec(), b"device-aux".to_vec()),
        (b"dir/COM1.log".to_vec(), b"device-com1".to_vec()),
        (b"nul.tar.gz".to_vec(), b"device-nul".to_vec()),
        (b"a:b|c?.txt".to_vec(), b"illegal-chars".to_vec()),
        (b"trail. ".to_vec(), b"trailing-dots".to_vec()),
        (b"../escape.txt".to_vec(), b"traversal".to_vec()),
        (b"/rooted.txt".to_vec(), b"absolute".to_vec()),
        (b"C:\\drive.txt".to_vec(), b"drive-prefix".to_vec()),
        (b"\\\\srv\\share\\unc.txt".to_vec(), b"unc".to_vec()),
        (long.into_bytes(), b"too-long".to_vec()),
        ("utf8-中文名.txt".as_bytes().to_vec(), b"non-ascii".to_vec()),
    ];
    let dirs: Vec<Vec<u8>> = vec![b"subdir/".to_vec()];
    let mut z = Zip::default();
    for (n, b) in &cases {
        z.add(n, b);
    }
    for d in &dirs {
        z.add(d, b"");
    }
    let path = std::env::temp_dir().join(format!("zn-{name}-{}.zip", std::process::id()));
    std::fs::write(&path, z.finish()).unwrap();
    (path, cases, dirs)
}

//! Stage-by-stage smoke probe for the 7z.dll FFI (debug aid).

use archive_core::dll;
use archive_core::com::vtables::InArchiveVt;
use archive_core::com::{
    Guid, CLSID_FORMAT_ZIP, IID_IIN_ARCHIVE, S_OK,
};

fn main() {
    eprintln!("[1] loading dll...");
    let dll = match dll::load() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[1] FAIL: {e}");
            return;
        }
    };
    eprintln!("[1] ok");

    eprintln!("[2] guid sizes: Guid={} InArchiveVt slots expect 13 ptrs",
        std::mem::size_of::<Guid>());
    eprintln!("    IInArchive IID bytes = {:02x?}", guid_bytes(&IID_IIN_ARCHIVE));
    eprintln!("    CLSID_ZIP    bytes = {:02x?}", guid_bytes(&CLSID_FORMAT_ZIP));

    eprintln!("[3] CreateObject...");
    let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe { dll.create_object(&CLSID_FORMAT_ZIP, &IID_IIN_ARCHIVE, &mut raw) };
    eprintln!("[3] hr=0x{:08X} ptr={:p}", hr as u32, raw);
    if hr != S_OK || raw.is_null() {
        return;
    }

    eprintln!("[4] building file stream...");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/plain.zip");
    let file = std::fs::File::open(&fixture).expect("open fixture");
    let stream = archive_core::com::instream::FileStreamOwner::new(file).expect("stream");
    eprintln!("[4] ok stream={:p}", stream.as_void());

    eprintln!("[5] building open callback...");
    let cb = archive_core::com::callbacks::OpenCallbackOwner::new(None);
    eprintln!("[5] ok cb={:p}", cb.as_void());

    eprintln!("[6] IInArchive::Open...");
    let vt = unsafe { &**(raw as *const *const InArchiveVt) };
    let hr = unsafe { (vt.open)(raw, stream.as_void(), std::ptr::null(), cb.as_void()) };
    eprintln!("[6] Open hr=0x{:08X}", hr as u32);

    eprintln!("[7] GetNumberOfItems...");
    let mut n: u32 = 0;
    let hr = unsafe { (vt.get_number_of_items)(raw, &mut n) };
    eprintln!("[7] hr=0x{:08X} n={}", hr as u32, n);

    eprintln!("[8] cleanup...");
    unsafe {
        (vt.close)(raw);
        (vt.release)(raw);
        stream.release_own();
    }
    drop(cb);
    eprintln!("[8] done");
}

fn guid_bytes(g: &Guid) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&g.data1.to_le_bytes());
    b[4..6].copy_from_slice(&g.data2.to_le_bytes());
    b[6..8].copy_from_slice(&g.data3.to_le_bytes());
    b[8..16].copy_from_slice(&g.data4);
    b
}


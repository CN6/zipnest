//! End-to-end tests for the downloader against a local HTTP server.
//!
//! These do not touch the network: each test starts a `TcpListener` on
//! 127.0.0.1:0 and speaks just enough HTTP/1.1 to exercise the cases that have
//! actually bitten — a truncated body with a normal `200`, a server that ignores
//! `Range`, a resumed transfer, and a mirror that serves the wrong bytes. The
//! hash check is the only thing standing between those and an installed
//! garbage file, so it is worth proving rather than reasoning about.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use zipnest_update::download::{download, CancelFlag};
use zipnest_update::manifest::Asset;
use zipnest_update::verify::sha256_hex;
use zipnest_update::UpdateError;

/// A one-request-at-a-time HTTP server with a fixed body and a behaviour knob.
struct Server {
    url: String,
    hits: Arc<AtomicUsize>,
}

#[derive(Clone, Copy)]
enum Behaviour {
    /// Honour `Range`, answer 206.
    Ranges,
    /// Ignore `Range`, always answer 200 with the whole body.
    IgnoreRange,
    /// Send only the first `n` bytes but claim the full `Content-Length` and
    /// close — the truncation that looks like success.
    TruncateAt(usize),
    /// Serve different bytes of the same length.
    WrongBytes,
}

fn serve(body: Vec<u8>, behaviour: Behaviour) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            hits2.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let start = req
                .lines()
                .find_map(|l| l.strip_prefix("range: bytes="))
                .or_else(|| {
                    req.lines()
                        .find_map(|l| l.strip_prefix("Range: bytes="))
                })
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);

            let payload = if matches!(behaviour, Behaviour::WrongBytes) {
                body.iter().map(|b| b.wrapping_add(1)).collect::<Vec<_>>()
            } else {
                body.clone()
            };
            let (status, slice, extra) = match behaviour {
                Behaviour::Ranges if start > 0 => (
                    "206 Partial Content",
                    &payload[start.min(payload.len())..],
                    format!(
                        "Content-Range: bytes {}-{}/{}\r\n",
                        start,
                        payload.len() - 1,
                        payload.len()
                    ),
                ),
                Behaviour::TruncateAt(n) => (
                    "200 OK",
                    &payload[..n.min(payload.len())],
                    String::new(),
                ),
                _ => ("200 OK", &payload[..], String::new()),
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n{extra}Connection: close\r\n\r\n",
                slice.len(),
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(slice);
            let _ = stream.flush();
        }
    });
    Server {
        url: format!("http://{addr}/file.exe"),
        hits,
    }
}

fn asset_for(body: &[u8], name: &str) -> Asset {
    Asset {
        name: name.to_string(),
        size: body.len() as u64,
        sha256: sha256_hex(body),
    }
}

fn data() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 251) as u8).collect()
}

#[test]
fn a_complete_download_lands_and_hashes() {
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    let asset = asset_for(&body, "a.exe");
    let mut last = (0, 0);
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |p| last = (p.done, p.total),
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(last, (body.len() as u64, body.len() as u64));
    // The .part is gone once the file has its final name.
    assert!(!dir.path().join("a.exe.part").exists());
}

#[test]
fn a_truncated_body_is_rejected_then_retried_from_the_next_source() {
    // The measured real-world failure: 1.4 MB of a 5 MB file, HTTP 200.
    let body = data();
    let bad = serve(body[..100_000].to_vec(), Behaviour::TruncateAt(100_000));
    let good = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    let asset = asset_for(&body, "b.exe");
    let path = download(
        &asset,
        &[bad.url.clone(), good.url.clone()],
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert!(bad.hits.load(Ordering::SeqCst) >= 1);
}

#[test]
fn a_mirror_serving_the_wrong_bytes_is_refused() {
    let body = data();
    let wrong = serve(body.clone(), Behaviour::WrongBytes);
    let dir = tempfile::tempdir().unwrap();
    let asset = asset_for(&body, "c.exe");
    let err = download(
        &asset,
        std::slice::from_ref(&wrong.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap_err();
    assert!(
        matches!(err, UpdateError::HashMismatch { .. }),
        "expected a hash failure, got {err:?}"
    );
    // Nothing may be left under the final name for a caller to execute.
    assert!(!dir.path().join("c.exe").exists());
}

#[test]
fn a_server_that_ignores_range_restarts_instead_of_appending() {
    // Pre-seed a partial file, then point at a server that answers 200 to the
    // Range request. Appending would produce twice the bytes; the correct
    // behaviour is to truncate and fetch the whole file.
    let body = data();
    let srv = serve(body.clone(), Behaviour::IgnoreRange);
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("d.exe.part");
    std::fs::write(&part, &body[..50_000]).unwrap();
    let asset = asset_for(&body, "d.exe");
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
}

#[test]
fn a_resumed_transfer_keeps_the_prefix() {
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("e.exe.part");
    std::fs::write(&part, &body[..80_000]).unwrap();
    let asset = asset_for(&body, "e.exe");
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
}

#[test]
fn a_finished_file_from_an_earlier_run_is_reused_without_the_network() {
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.exe"), &body).unwrap();
    let asset = asset_for(&body, "f.exe");
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(
        srv.hits.load(Ordering::SeqCst),
        0,
        "a verified file on disk must not be downloaded again"
    );
}

#[test]
fn a_stale_file_of_the_right_size_but_wrong_content_is_replaced() {
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    let stale = body.iter().map(|b| b.wrapping_add(1)).collect::<Vec<_>>();
    std::fs::write(dir.path().join("g.exe"), &stale).unwrap();
    let asset = asset_for(&body, "g.exe");
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn a_full_length_part_with_wrong_bytes_is_refetched_not_trusted() {
    // The state a mirror serving right-length/wrong-bytes leaves behind, and
    // the state a cancel in the final read cycle leaves behind. Returning
    // success for it on the file's length alone made every later source look
    // successful without a request, so the mirror list was walked without
    // downloading anything and the user got a hash error nothing could fix.
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    let stale = body.iter().map(|b| b.wrapping_add(1)).collect::<Vec<_>>();
    let part = dir.path().join("i.exe.part");
    std::fs::write(&part, &stale).unwrap();
    assert_eq!(stale.len(), body.len(), "test premise: same length");
    let asset = asset_for(&body, "i.exe");
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(
        srv.hits.load(Ordering::SeqCst),
        1,
        "the local file must be re-fetched, not accepted"
    );
}

#[test]
fn a_full_length_part_with_the_right_bytes_is_adopted_without_a_request() {
    // The other half: a finish-then-cancel leaves a perfectly good file, and
    // making the user download 5 MB again would be worse than the bug.
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("j.exe.part"), &body).unwrap();
    let asset = asset_for(&body, "j.exe");
    let path = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(
        srv.hits.load(Ordering::SeqCst),
        0,
        "a correct full-length .part must not be downloaded again"
    );
}

#[test]
fn a_cancel_before_the_first_byte_leaves_nothing_behind() {
    let body = data();
    let srv = serve(body.clone(), Behaviour::Ranges);
    let dir = tempfile::tempdir().unwrap();
    let asset = asset_for(&body, "h.exe");
    let flag = CancelFlag::default();
    flag.cancel();
    let err = download(
        &asset,
        std::slice::from_ref(&srv.url),
        dir.path(),
        |_| {},
        || flag.is_cancelled(),
    )
    .unwrap_err();
    assert!(matches!(err, UpdateError::Cancelled));
    assert!(!dir.path().join("h.exe").exists());
}
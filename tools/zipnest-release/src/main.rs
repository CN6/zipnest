//! `zipnest-release` — release-side tooling.
//!
//! ```text
//! zipnest-release keygen                 generate the update key (once, ever)
//! zipnest-release pubkey                 print the key the app must embed
//! zipnest-release sign <file>            write <file>.sig
//! zipnest-release verify <file>          check <file>.sig against the app's key
//! zipnest-release manifest ...           build update.json from two artifacts
//! ```
//!
//! Everything here is run by `scripts\ship.ps1` from the maintainer's machine,
//! right after the artifacts are built and before anything is published.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zipnest_release as rel;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        usage();
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];
    match run(cmd, rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zipnest-release: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "usage:\n  zipnest-release keygen [--out <path>]\n  zipnest-release pubkey \
         [--key <path>]\n  zipnest-release sign <file> [--key <path>]\n  zipnest-release verify \
         <file> [--key <path>]\n  zipnest-release manifest --version <v> --installer <exe> \
         --portable <zip> [--notes-zh <s>] [--notes-en <s>] [--min-version <v>] [--date \
         <YYYY-MM-DD>] [--out <update.json>]"
    );
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn run(cmd: &str, args: &[String]) -> Result<(), String> {
    match cmd {
        "keygen" => {
            let path = flag(args, "--out")
                .map(PathBuf::from)
                .unwrap_or_else(rel::default_key_path);
            let (pk, path) = rel::generate_key(&path)?;
            println!("wrote {}", path.display());
            println!("public key (hex): {}", rel::hex(&pk));
            println!();
            println!("Put that hex into UPDATE_PUBKEY in");
            println!("  crates/zipnest-update/src/verify.rs");
            println!("and keep {} off every backup that leaves your machine.", path.display());
            Ok(())
        }
        "pubkey" => {
            let path = flag(args, "--key")
                .map(PathBuf::from)
                .unwrap_or_else(rel::default_key_path);
            println!("{}", rel::hex(&rel::public_key_of(&path)?));
            Ok(())
        }
        "sign" => {
            let Some(file) = args.first().filter(|a| !a.starts_with("--")) else {
                return Err("sign needs a file".into());
            };
            let key = flag(args, "--key")
                .map(PathBuf::from)
                .unwrap_or_else(rel::default_key_path);
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let sig = rel::sign_with_key_file(&key, &bytes)?;
            let out = PathBuf::from(format!("{file}.sig"));
            std::fs::write(&out, &sig).map_err(|e| format!("{}: {e}", out.display()))?;
            // Re-verify immediately: a signature that does not match the key the
            // app embeds is worse than no signature, because the release looks
            // published and every client silently refuses it.
            let pk = rel::public_key_of(&key)?;
            zipnest_update::verify::verify_with(&pk, &bytes, &sig)
                .map_err(|e| format!("internal: fresh signature does not verify: {e}"))?;
            if pk != zipnest_update::verify::UPDATE_PUBKEY {
                eprintln!(
                    "warning: this key is NOT the one compiled into the app.\n  signing key:  {}\n  app embeds:   {}\nThe signature will be rejected by every client.",
                    rel::hex(&pk),
                    rel::hex(&zipnest_update::verify::UPDATE_PUBKEY)
                );
            }
            println!("wrote {} ({} bytes)", out.display(), sig.len());
            Ok(())
        }
        "verify" => {
            let Some(file) = args.first().filter(|a| !a.starts_with("--")) else {
                return Err("verify needs a file".into());
            };
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let sig = std::fs::read(format!("{file}.sig")).map_err(|e| format!("{file}.sig: {e}"))?;
            let pk = match flag(args, "--key") {
                Some(p) => rel::public_key_of(Path::new(p))?,
                None => zipnest_update::verify::UPDATE_PUBKEY,
            };
            zipnest_update::verify::verify_with(&pk, &bytes, &sig)
                .map_err(|e| format!("signature rejected: {e}"))?;
            println!("ok: {} verifies against {}", file, rel::hex(&pk));
            Ok(())
        }
        "manifest" => run_manifest(args),
        other => Err(format!("unknown command {other:?}")),
    }
}

/// Build `update.json` from the two artifacts, computing their hashes from the
/// files themselves so the manifest cannot disagree with what is uploaded.
fn run_manifest(args: &[String]) -> Result<(), String> {
    let version = flag(args, "--version").ok_or("--version is required")?;
    let installer = flag(args, "--installer").ok_or("--installer is required")?;
    let portable = flag(args, "--portable").ok_or("--portable is required")?;
    let out = flag(args, "--out").unwrap_or("update.json");

    let mut assets = Vec::new();
    for path in [installer, portable] {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| format!("{path}: no file name"))?;
        let size = std::fs::metadata(path)
            .map_err(|e| format!("{path}: {e}"))?
            .len();
        let sha256 = zipnest_update::verify::sha256_file(Path::new(path))
            .map_err(|e| format!("{path}: {e}"))?;
        assets.push(serde_json::json!({
            "name": name,
            "size": size,
            "sha256": sha256,
        }));
    }

    // Serialised with the field order declared here and no trailing newline:
    // the signature covers these exact bytes, and a pretty-printer change would
    // invalidate every manifest if the app ever re-serialised (it does not —
    // see verify::verify_manifest's contract).
    let manifest = serde_json::json!({
        "version": version,
        "min_version": flag(args, "--min-version").unwrap_or(""),
        "date": flag(args, "--date").unwrap_or(""),
        "notes_zh": flag(args, "--notes-zh").unwrap_or(""),
        "notes_en": flag(args, "--notes-en").unwrap_or(""),
        "assets": assets,
    });
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    // Parse it back through the *client's* validator rather than trusting the
    // shape we just built: this is the same code path a user's app will run.
    zipnest_update::manifest::Manifest::parse(text.as_bytes())
        .map_err(|e| format!("generated manifest does not validate: {e}"))?;
    std::fs::write(out, text.as_bytes()).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}");
    for a in manifest["assets"].as_array().into_iter().flatten() {
        println!(
            "  {}  {}",
            zipnest_update::pretty_size(a["size"].as_u64().unwrap_or(0)),
            a["name"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

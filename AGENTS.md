# AGENTS.md

ZipNest — full-featured archive manager for Windows (x64 + ARM64), built on the
official 7-Zip engine. This file carries the invariants a fresh session would
otherwise violate. Read it first, every session.

## Where things are

- `docs/superpowers/specs/2026-09-30-zipnest-design.md` — architecture, IPC contract, scope per milestone.
- `docs/superpowers/plans/` — the active milestone's step-by-step plan (read the current one before coding).
- `README.md` — build/test commands, fixtures, current status.

## Invariants

**Engine**
- Formats: ZIP / 7Z / RAR / TAR / GZ / BZ2 / XZ / ISO extract; ZIP / 7Z / TAR create. RAR is extract-only — creating RAR is a licensed format, so it stays out.
- `7z.dll` is loaded dynamically (LGPL compliance): ship the unmodified official binary, resolve it at runtime, and let the user replace it.

**Security**
- Every archive entry path passes through `archive_security::sanitize_entry_path` before it touches the filesystem.
- Extraction writes are bounded by `ExtractQuota` (zip-bomb guard).
- Passwords live in memory only: redacted from `Debug`, and they never reach logs or disk.

**Boundaries**
- Errors cross layers only as `ZipnestError` (Rust) or `IpcError { key }` (frontend). Keys are i18n strings; every key has both a zh-CN and an en-US entry.
- The frontend never touches the filesystem — everything goes through IPC commands.
- A progress callback returns `false` to cancel, which maps to `ZipnestError::Cancelled`.

**Product**
- Donation model: every feature is free. No licensing, activation, trial, or feature-gate subsystem — ever.
- No telemetry. Logs are local only.

## Workflow

- Plan first: for non-trivial work, brainstorm → write the plan under `docs/superpowers/plans/` → execute it.
- Test first, per task. A task is done only when `cargo test --workspace` is green and `pnpm test` passes; clippy stays warning-free.
- End each task with an English commit. Tag each finished milestone (`m1-engine`, `m2-ui`, …).

## Dev-loop gotchas

- PowerShell 5.1 starts each shell plain: run `[Console]::OutputEncoding=[Text.Encoding]::UTF8` and prepend `$env:USERPROFILE\.cargo\bin` to `$env:Path` before cargo/pnpm.
- `tauri dev` watches `crates/**`: editing Rust restarts the app and wipes UI state; front-end edits hot-reload.
- The window position changes on every restart — re-snapshot coordinates before driving the UI.

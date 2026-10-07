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

## Shipping a release (Windows) - exact recipe

`apps/zipnest-native` is the real app (egui, no WebView2); `apps/zipnest` is the
older web UI and is NOT shipped.

1. Bump `CURRENT_VERSION` in `apps/zipnest-native/src/main.rs`, `!define VERSION`
   and `OutFile` in `packaging/installer.nsi`, and the version/file names in
   `README.md`. Add a `CHANGELOG.md` section (Chinese, matching that file's style).
2. `cargo test --workspace` (all suites green) + `cargo clippy --workspace
   --all-targets` (no new warnings).
3. `cargo build -p zipnest-native --release`, then copy
   `target\release\zipnest-native.exe` over `dist-portable\ZipNest\zipnest.exe`.
4. Installer: run `%LOCALAPPDATA%\tauri\NSIS\makensis.exe /V2 installer.nsi` from
   `packaging\` (NSIS is the copy bundled with Tauri; there is no system install).
   `installer.nsi` must keep its UTF-8 BOM or makensis aborts with "Bad text
   encoding".
5. Portable: `Compress-Archive dist-portable\ZipNest` into
   `ZipNest_<ver>_x64_portable.zip` (installer + zip are gitignored).
6. Update the machine: close any running instance first (the file is locked),
   then copy the exe over `D:\ZipNest\zipnest.exe`.
7. Commit (English message), push master, `git tag -f -a v<ver>`, push the tag
   with `+refs/tags/v<ver>`, then `gh release create v<ver> <installer> <zip>
   --title ... --notes-file ...`.

## Known open items / gotchas (as of v0.4.8)

- **File associations are claimed automatically** (v0.4.8). Three pieces, all
  HKCU: `assoc_ops` (ProgID per extension + the extension default +
  `OpenWithProgids` + `Applications\zipnest.exe` + `RegisteredApplications` /
  `Capabilities`), the installer's `zipnest.exe --register-integration`, and a
  one-time first-launch bootstrap guarded by `Settings::integration_applied`.
  Registering from the Settings dialog only (<= v0.4.7) is why a fresh machine
  was never the default. The two flags default to **true** now.
- **A foreign `UserChoice` cannot be taken over — do not try.** Measured on
  Windows 10 26H1: `HKCU\...\Explorer\FileExts\.<ext>\UserChoice` carries a
  Deny-SetValue ACE for the user, so writing `ProgId` fails ("不允许所请求的
  注册表访问权") and `reg delete /f` on the key reports access denied. Only the
  user can change it, via 设置 → 默认应用. `shell::user_choice_blocks` /
  `blocked_extensions` report those extensions and the Settings dialog deep-links
  to `ms-settings:defaultapps?registeredAppUser=ZipNest`. Never forge the hash.
- **`assoc:double-click` smoke test**: `cmd /c start "" x.7z` must launch
  `zipnest.exe` from an extension with no `UserChoice`. Shell-level probes are
  unreliable on this box: `assoc .ext` says "not found" and
  `AssocQueryStringW(ASSOCSTR_EXECUTABLE)` returns `0x80070483` even for `.txt`,
  so verify against the registry itself.
- **settings.json tolerates a leading UTF-8 BOM** (v0.4.8). Notepad writes one
  by default; before the fix the whole document failed to parse, the store fell
  back to defaults and the next save wrote those over every preference. Note
  this shell is **Windows PowerShell 5.1**: `Set-Content -Encoding UTF8` adds a
  BOM, so use the `write` tool or `-Encoding utf8NoBOM` for JSON fixtures here.
- **Startup flash**: Windows applies the window size roughly 0.3 s after the
  window becomes visible, so a 16x16 square can be seen briefly on restore.
  Creating the window hidden (`with_visible(false)`) and showing it with
  `ViewportCommand::Visible(true)` on the first frame did NOT remove it here.
  Maximized state is deliberately NOT remembered: restoring it flashed
  small-first. Timing the show with Win32 `ShowWindow` is the next idea.
- **Geometry units**: egui viewport rects are native points, so saving and
  restoring must BOTH use `system_dpi_scale()`. `pixels_per_point()` includes the
  UI zoom and makes the window grow/shrink on every launch.
- **`WINDOW_SIZE` minimum must stay small** (400): a 760 floor silently clamped
  every shorter window height, so resized windows were never remembered.
- **Context-menu registration**: fails where an explicit Deny ACE sits on
  `HKCU\Software\Classes\*\shell` or `...\Directory\Background\shell` (even an
  elevated admin is refused). `AllFilesystemObjects\shell` stays writable and is
  the documented fallback; the Win11 MSIX menu is unaffected either way. Warning
  key: `error.shell.file_menu`.
- **Speed**: ZIP's Deflate is single-threaded, so `mt=on` only helps 7z/LZMA2.
  Progress is throttled to one report per 50 ms because the engines report once
  per entry and every report wakes the UI thread.
- **Verification habit**: inspect the *unfiltered* window state. A sampling
  script that ignored windows narrower than 100 px once produced a false
  "no flash" conclusion.
- **Dialogs** intentionally share the main window's text scale; do not reintroduce
  a per-dialog font scale.
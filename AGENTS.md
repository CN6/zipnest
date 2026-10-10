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

- **CHANGELOG.md is written for users, not for us.** Every entry must be
  something a user can perceive in the app: a bug they could hit, a feature they
  can see, a behaviour change. Build tooling, tests, scripts, refactors,
  duplicate keys, compiler warnings, CI — none of that belongs there; it goes in
  the commit message. If a fix cannot be noticed from outside, leave it out. Keep
  the plain-Chinese, "什么问题 → 现在怎样" voice the file already uses.
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
   and `OutFile` in `packaging/installer.nsi`, `Version` in
   `packaging/win11-shell/AppxManifest.xml`, and the version/file names in
   `README.md`. Add a `CHANGELOG.md` section (Chinese, matching that file's style).
   Then run `packaging\check-versions.ps1`: it fails loudly on any of the six
   places disagreeing, which is how the Win11 manifest version sat at 0.4.0.0 for
   ten releases unnoticed.
2. `cargo test --workspace` (all suites green) + `cargo clippy --workspace
   --all-targets` (no new warnings). CI (`.github/workflows/ci.yml`) runs the
   shipped crates only — `apps/zipnest` is the unshipped WebView2 UI and would
   drag a second desktop stack into every run.
3. Build both binaries: `cargo build -p zipnest-native --release` and
   `cargo build -p zipnest-shell --release` (the shell DLL feeds the Win11 MSIX).
4. Emit the payload: `packaging\emit-portable.ps1 -ThirdParty`. It assembles
   `dist-portable\ZipNest\` from tracked sources (exe, `vendor\7zip-bin\7z.dll`,
   the SFX stubs, license texts) and calls `stage-portable.ps1`, which builds and
   signs `Win11Shell\ZipNestShell.msix` with the self-signed cert in
   `Cert:\CurrentUser\My` (CN=ZipNest). Nothing else creates that directory: it is
   gitignored, which before v0.4.11 meant a clean checkout could not build an
   installer at all.
5. Installer: run `%LOCALAPPDATA%\tauri\NSIS\makensis.exe /V2 installer.nsi` from
   `packaging\` (NSIS is the copy bundled with Tauri; there is no system install).
   `installer.nsi` must keep its UTF-8 BOM or makensis aborts with "Bad text
   encoding".
6. Portable: `Compress-Archive dist-portable\ZipNest` into
   `ZipNest_<ver>_x64_portable.zip` (installer + zip are gitignored).
7. Update the machine: close any running instance first (the file is locked),
   then copy the exe over `D:\ZipNest\zipnest.exe`.
8. Release (title and body have a fixed shape — do not append the summary):
   - Title is exactly `ZipNest v<ver>`. The "solved what" line does NOT belong
     here: the page shows the version, and the body below IS the changelog.
     Putting the summary in both places was the v0.4.0–v0.4.9 habit and it read
     as a duplicate.
   - Notes file = the CHANGELOG section for this version **with its first
     heading line removed** (that heading repeats the version + summary). So the
     body starts straight at `### 修复` / `### 新增`.
   - `gh release create v<ver> <installer> <zip> --title "ZipNest v<ver>"
     --notes-file <notes>`. To fix a title afterwards it is
     `gh release edit <tag> --title "..."` — `--name` does not exist, even
     though `gh release view --json` calls the field `name`.
   - Then `git tag -f -a v<ver>`, push with `+refs/tags/v<ver>`, `gh release
     create`. If a release must be re-cut minutes later:
     `gh release delete <tag> --yes --cleanup-tag`, re-tag, re-create.
   - Local installers/portable zips are not kept: upload them and delete the
     copies (the maintainer does not want them on disk).

## Known open items / gotchas (as of v0.4.11)

- **Scripts and JSON this agent writes**: the editor tool drops a leading `#`
  from the first line (`#Requires -Version 5.1` came out as
  `Requires -Version 5.1` and PowerShell refused to parse it) and strips a UTF-8
  BOM. Both matter here: PowerShell 5.1 reads a BOM-less `.ps1` as ANSI, so
  Chinese literals/regexes silently break, and `installer.nsi` needs its BOM or
  makensis aborts. After writing a script, check the first bytes and the first
  line.
- **A default handler is per extension and Windows guards it** (v0.4.11). The
  app cannot take over an extension whose `UserChoice` names another program, so
  the Settings dialog's 「一键设为默认」 calls `shell::launch_default_apps_ui()`
  and falls back to `ms-settings:defaultapps?registeredAppUser=ZipNest`, which is
  the route that actually works (verified: it launches SystemSettings). The
  classic `IApplicationAssociationRegistrationUI::LaunchAdvancedAssociationUI`
  is best-effort: measured on Windows 10 build 28020 it returns E_INVALIDARG
  (0x80070057) for **every** application name, machine-registered ones like
  "Microsoft Edge" included, and it blocks until its page is closed when it does
  work — hence the worker thread and the silent fallback. The installer also
  writes `HKLM\Software\RegisteredApplications\ZipNest` + `Capabilities` so the
  manual 默认程序 list still shows ZipNest on systems that have that page; it is
  not what makes the one-click button work. Never try to forge or delete
  `UserChoice`.
- **Opening an archive pins it until it is closed** (v0.4.11). `Archive` frees
  its 7z handler, source file handle and password in `Drop`, and
  `ArchiveRegistry` had no removal path at all — a browsing session leaked one
  per opened file. The UI calls `IpcService::close_archive` when it replaces an
  archive; anything else that opens archives must do the same.
- **The installer must never wait on zipnest.exe** (v0.4.8 shipped exactly that
  hang). `nsExec::ExecToLog` blocks until the child exits *and* closes its output
  pipe; v0.4.8 ran `zipnest.exe --register-integration` through it, and on a
  machine where the file copy was skipped (`SetOverwrite try` skips a locked file
  silently) that child was a **stale build** that does not know the flag — it
  treats it as "no arguments" and opens its main window, so the installer waited
  forever on the shortcut screen. Now: `Exec` (start, never wait), a
  `GetDLLVersion` guard so an unreplaced exe is not called at all, a registry
  read-back that also checks `UserChoice` before claiming success, a
  `DetailPrint` per step (so a screenshot of the Details pane names the stuck
  step), `SetErrorLevel` on every failure path, and `cap_cli_runtime` makes both
  CLI modes self-terminate after 20 s.

- **The finish page owns the desktop shortcut** (v0.4.10). "创建桌面快捷方式"
  is MUI's *readme* checkbox repurposed through
  `MUI_FINISHPAGE_SHOWREADME_FUNCTION`; `MUI_FINISHPAGE_SHOWREADME` still has to
  be defined (empty) because Finish.nsh wraps the whole block, FUNCTION branch
  included, in `!ifdef MUI_FINISHPAGE_SHOWREADME`. The install section creates
  only the Start-menu shortcuts, so clearing the box really means "no desktop
  icon"; an upgrade never deletes a shortcut the user already had.
- **Update checks are silent unless they have news** (v0.4.10).
  `spawn_update_check(state, manual)`: the manual call sets `Checking` and
  reports failures, the startup call touches no state while it runs and only
  ever writes `Found`. Before this, `Checking` was set unconditionally, so every
  launch flashed the "正在检查更新" window and an offline machine got an error
  dialog.

- **Feedback entry** (v0.4.9): 设置 → 帮助与反馈. `zipnest_ipc::diagnostics`
  renders the report (version, OS build, exe path with the account name masked to
  `%USERPROFILE%`, integration flags, engine state, last error key) and builds
  the `mailto:` URL. It must never gain a file name, an archive entry or a user
  path; the tests in that module are the guard. Nothing is transmitted
  automatically — clipboard and `mailto:` only, so the "no telemetry" rule holds.
- **File associations are claimed automatically** (v0.4.8). Three pieces, all
  HKCU: `assoc_ops` (ProgID per extension + the extension default +
  `OpenWithProgids` + `Applications\zipnest.exe` + `RegisteredApplications` /
  `Capabilities`), the installer's `zipnest.exe --register-integration`, and a
  one-time first-launch bootstrap guarded by `Settings::integration_applied`.
  Registering from the Settings dialog only (<= v0.4.7) is why a fresh machine
  was never the default. The two flags default to **true** now. The decision
  lives in `integration_plan` (zipnest-native/main.rs): never-claimed → claim
  with defaults whatever the old flags hold; recorded → apply as-is; the
  installer's flag forces a re-apply, a normal launch never does (that is what
  makes an opt-out stick).
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
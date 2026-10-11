# AGENTS.md

ZipNest — full-featured archive manager for Windows (x64 + ARM64), built on the
official 7-Zip engine. This file carries the invariants a fresh session would
otherwise violate. Read it first, every session.

## Where things are

- `docs/superpowers/specs/2026-09-30-zipnest-design.md` — architecture, IPC contract, scope per milestone.
- `docs/superpowers/plans/` — the active milestone's step-by-step plan (read the current one before coding).
- `docs/handoff-2026-10-12.md` — dated handoff snapshot (standing user requirements, release recipe). AGENTS.md wins wherever the two disagree.
- `README.md` — build/test commands, fixtures, current status.

## Invariants

**Engine**
- Formats: ZIP / 7Z / RAR / TAR / GZ / BZ2 / XZ / ISO extract; ZIP / 7Z / TAR create. RAR is extract-only — creating RAR is a licensed format, so it stays out.
- `7z.dll` is loaded dynamically (LGPL compliance): ship the unmodified official binary, resolve it at runtime, and let the user replace it.

**Security**
- Extraction resolves *every* entry path through `archive_security::plan_entry_paths` before `Extract` starts: the planner rewrites whatever Windows cannot store and never drops an entry (see "Nothing is ever lost" below). `sanitize_entry_path` (strict, rejects) stays the guard for names *we* write into an archive.
- Extraction writes are bounded by `ExtractQuota` (zip-bomb guard).
- Passwords live in memory only: redacted from `Debug`, and they never reach logs or disk.

**Nothing is ever lost (v0.4.15)**
- A run that reports success must have written every selected entry. `ExtractStats::failed` non-empty ⇒ the job must end as a failure (`error.entries_failed`) and the UI must list which entries failed and why; never a "完成" message over missing files.
- Weird names are *rewritten*, not skipped: device names (`CON` → `CON_`), illegal characters, trailing dots, `..`/drive/UNC prefixes, over-long components (cut to 255 bytes, extension kept), and collisions (`a.txt`/`A.TXT`, or two names that clean to the same thing) all get a safe name and are reported in `ExtractStats::renamed`.
- Genuine per-entry write failures record `failed` (reason key + the OS's own words) and extraction continues, so the good files still land — the *run* is what fails.
- Windows paths past `MAX_PATH` need the verbatim prefix: every fs call in `extract.rs` goes through `os_path()`. Without it a >260-char destination path fails with `os error 123`.

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
4. Emit the payload: `packaging\emit-portable.ps1` (third-party licenses are
   generated by default; the switch that skips them is `-NoThirdParty`). It
   assembles
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

## Known open items / gotchas (as of v0.4.15)

- **Scripts and JSON this agent writes**: the editor tool drops a leading `#`
  from the first line (`#Requires -Version 5.1` came out as
  `Requires -Version 5.1` and PowerShell refused to parse it) and strips a UTF-8
  BOM. Both matter here: PowerShell 5.1 reads a BOM-less `.ps1` as ANSI, so
  Chinese literals/regexes silently break, and `installer.nsi` needs its BOM or
  makensis aborts. After writing a script, check the first bytes and the first
  line.
- **Never round-trip a repo file through PowerShell's text cmdlets**: `Get-Content`
  / `Set-Content` default to ANSI here (GBK), so `(Get-Content x -Raw) -replace …
  | Set-Content -Encoding UTF8 x` turned every em dash in `AGENTS.md` and
  `user_choice.rs` into `鈥?`. Use the `edit`/`write` tools, or .NET with an
  explicit `UTF8Encoding($true/$false)` and the BOM state you checked first.
- **A default handler is per extension, and since v0.4.13 ZipNest takes it over
  itself** — `crates/zipnest-ipc/src/user_choice.rs`. What the OS actually
  follows is `HKCU\...\Explorer\FileExts\.<ext>\UserChoice`, which carries a
  `Deny SetValue` ACE (writes fail) **and** a `Hash` Windows re-computes before
  honouring the entry. The ACE does not deny `DELETE` and the user owns the key,
  so the working recipe is the one Firefox ships: **delete the key, recreate it
  with our `ProgId`, write a hash we compute from the key's own last-write time**
  (salt `User Choice set via Windows User Experience {D18B6DD5-…}`, MD5 over the
  lowercased UTF-16LE message including its NUL, then the bespoke scramble).
  Measured on Windows 11 26H1 (build 28020): the computed hash reproduced the
  stored hash of **every** `UserChoice` on the machine (8/8 here, 155/155 in an
  independent port), and afterwards the shell opened `.zip`/`.7z` with ZipNest
  and no dialog. Things that are easy to get wrong, all measured:
  - the extension in the hash input **includes the dot** (`.tar`, not `tar`);
    without it Windows rejects the entry and then *deletes* the whole key;
  - only the key's **minute** matters, so re-read its last-write time after
    writing and re-verify — a write that lands in the next minute produces a
    value Windows throws away;
  - `UserChoiceLatest` (a second, undocumented store some 26H1 builds add) holds
    the choice in a `ProgId` **subkey**, so removing it needs `RegDeleteTreeW`,
    not `RegDeleteKeyW`;
  - never claim victory by comparing the value you just wrote: recompute;
  - `UCPD.sys` ("User Choice Protection Driver") is running, and its v4.8
    protected set covers http/https, `.pdf`, `.html`/`.htm` and the Office
    formats — **none of our archive extensions**. Do not extend the takeover to
    anything on that list;
  - the "show the user a Windows page" route is dead: Microsoft documents that
    `SHOpenWithDialog` cannot set a default since Windows 10, and
    `LaunchAdvancedAssociationUI` returns E_INVALIDARG for every name on build
    28020. Sending users to `ms-settings:defaultapps` just moves the puzzle to
    them; v0.4.12 did that and v0.4.13 removed it.
- **Settings is a page of the main window, not a window of its own** (v0.4.14).
  `settings_page()` draws *instead of* the archive view: it reuses the toolbar's
  and the status bar's panel ids (`TopBottomPanel::top("toolbar")` /
  `bottom("status")`) so the Back row and the Save/Cancel footer take those seats
  over instead of stacking extra bars, and `update()` branches on
  `self.show_settings` around the three panels. The scroll body no longer caps its
  own height (`available_height() - 44`) — the footer is a real panel now, so the
  body cannot push it out of view.
  The two dead ends this replaced, both worth remembering: an embedded
  `egui::Window` is clipped by the archive list and can lose its bottom edge, and
  v0.4.13's attempt to fix that with `Context::show_viewport_immediate` produced a
  genuinely separate OS window — which users read as "a second program opened",
  plus it is created **behind** the main window on Windows and needs
  `ViewportCommand::Focus` and its own background fill (eframe clears immediate
  viewports with `[0,0,0,0]`, not the app's clear colour). A page needs none of
  that, which is why it won.
- **`shell_register` refuses a `target/` exe path** (`error.shell.dev_path`), so a
  `cargo run` / `cargo test` build can never become the system handler. To exercise
  the real registration path by hand, copy the binary somewhere else first
  (`%TEMP%\…`) — that is also why `--register-integration` prints nothing and
  exits 0 on a dev build.
- **Packaging scripts must tolerate cargo's stderr**: `build-package.ps1` sets
  `$ErrorActionPreference = 'Stop'`, and PowerShell 5.1 turns a native command's
  stderr into a terminating error — so the benign LNK4104 linker warning on the
  shell DLL's COM exports aborted the MSIX build and left the portable payload
  incomplete. The cargo call is wrapped with a local `Continue` preference.
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
- **A foreign `UserChoice` IS taken over, and it happens in code — not by sending
  the user to Settings.** The v0.4.12 note that this was impossible is obsolete:
  `crate::user_choice::claim_all` deletes the subkey and recreates it with a hash
  we compute (measured details are in the section above). Two things produced that
  wrong conclusion and are still worth knowing: the `Deny SetValue` ACE stops
  *writes* only, so `reg delete /f` on the key still reports access denied even
  though the Win32 delete with DELETE access succeeds — the two are not
  interchangeable here; and `shell::user_choice_blocks` / `blocked_extensions`
  are now only a *status classifier* (they feed `blocked` in the associate result
  and the `settings.shell.blocked` line, whose text says the button below fixes
  it). They are not a verdict that the extension is out of reach. The
  `ms-settings:` deep link is gone from the tree.
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

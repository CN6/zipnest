# v0.4.8 — ZipNest is the default archive handler out of the box

## Problem (reported on v0.4.7)

On a **fresh Windows machine** ZipNest is never the default archive program after
installing. It only becomes the default on the maintainer's box because the
"默认用 ZipNest 打开压缩包" checkbox was ticked there by hand once
(`%APPDATA%\ZipNest\settings.json` → `"associate": true`).

Measured facts on the v0.4.7 install:

| Where | State |
| --- | --- |
| `packaging/installer.nsi` | writes no `HKCU\Software\Classes` association keys at all; only the Win11 MSIX menu is registered |
| `Settings::default()` | `associate: false`, `context_menu: false` (`crates/zipnest-ipc/src/settings.rs`) |
| app startup | never registers anything; only the Settings dialog's *Save* button calls `shell_register` |
| `HKCU\Software\RegisteredApplications` | no `ZipNest` entry → ZipNest is invisible to Windows "默认应用" |
| `HKCU\...\FileExts\.zip\UserChoice` | `Deny SetValue` ACE for the user; `Set-ItemProperty` → *access denied*; `reg delete /f` → *access denied* |

So two independent defects:

1. **Nothing claims the associations.** The installer and the first launch of the
   app both leave `HKCU\Software\Classes\.<ext>` unset, so Windows falls back to
   the built-in `CompressedFolder` handler.
2. **A foreign `UserChoice` cannot be overwritten programmatically** (confirmed
   above, and it is the documented Windows 10/11 behaviour since the hash
   protection of `UserChoice`). Writing to it fails **and deleting the key
   fails**, so the only correct remaining move is to make ZipNest visible in
   Windows' own chooser and deep-link the user there.

## Fix

### A. Claim the associations by default

- `shell::assoc_ops` grows the pieces Windows needs to treat ZipNest as a
  first-class handler:
  - `HKCU\Software\Classes\Applications\zipnest.exe\shell\open\command`
    (+ `FriendlyAppName`, `SupportedTypes\.<ext>`) so ZipNest shows up in
    "打开方式" / "Open with" with its own name;
  - `HKCU\Software\ZipNest\Capabilities` (`ApplicationName`,
    `ApplicationDescription`, `FileAssociations\.<ext>` → `ZipNest.<ext>`) and
    `HKCU\Software\RegisteredApplications\ZipNest` → that path, which is what
    makes ZipNest appear in 设置 → 应用 → 默认应用 and get the
    "set this program as default" button.
- `Settings::default()` → `associate: true`, `context_menu: true`.
- The app registers once on first run: new persisted flag
  `integration_applied`; while it is `false` the app applies the current
  settings' integration (so a user who had explicitly turned it off stays
  opted out) and then sets the flag, which stops any re-registration.
- The installer runs `zipnest.exe --register-integration` right after copying
  the files, so a machine that never launches ZipNest is still associated.
- Uninstall keeps using `--unregister-shell`, extended to drop the new keys.

### B. Be honest about a `UserChoice` that belongs to somebody else

- New `AssocProbe` trait (`user_choice_progid(ext)`) with the real
  `WindowsRegistry` implementation; tests inject fakes.
- `shell_register` reports `blocked: Vec<String>` — the extensions where Windows
  has locked in another handler.
- Settings → 集成 shows that state in plain words and offers a button that opens
  `ms-settings:defaultapps?registeredAppUser=ZipNest` (Win11 per-app page; on
  Windows 10 the same URI opens the Default apps page).

### C. Nothing that cannot work is attempted

Deleting/hashing `UserChoice` is not performed: measured access-denied here, it
is a Windows-protected boundary, and a forged hash would look like association
hijacking to AV. The plan is: claim what can be claimed, and hand the user the
one-click system page for the rest.

## Test plan

- `shell.rs`: ops contain the capability/app keys; removals delete exactly them
  and nothing else; `user_choice_blocks` truth table; `blocked_extensions`
  against a fake probe.
- `service.rs`: `shell_register` surfaces `blocked`; existing partial-denial
  behaviour unchanged.
- `settings.rs`: defaults are opt-in-by-default; `integration_applied` patch and
  round-trip; sanitize keeps the flag.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets`.
- Manual: run `zipnest.exe --register-integration` on this machine and confirm
  the keys, then confirm a fresh-profile simulation (delete the 9 Classes
  defaults, re-register, `.7z` resolves to `ZipNest.7z`).

## Release

Version `0.4.8` in `main.rs`, `packaging/installer.nsi` (`VERSION`, `OutFile`),
`README.md`; new `CHANGELOG.md` section; AGENTS.md "open items" gets the
measured `UserChoice` facts.

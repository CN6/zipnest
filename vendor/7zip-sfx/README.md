# Vendored 7-Zip SFX stubs

Self-extracting-archive stubs used by `archive-core`'s `create_archive` when
`CreateOptions::sfx` is set. The engine builds a normal 7z archive and prepends
one of these stubs, yielding a runnable `<stub> + <7z>` `.exe`.

## Source

- Origin: `C:\Program Files\7-Zip` (official 7-Zip distribution)
- Version: **7-Zip 26.03 (x64)** — `7-Zip 26.03 (x64) : Copyright (c) 1999-2026 Igor Pavlov : 2026-09-03`
- Copied verbatim; the binaries are **unmodified**, matching the project's
  dynamic/redistributable 7-Zip policy.

| File | Bytes | Purpose |
|------|-------|---------|
| `7z.sfx` | 215552 | GUI extractor stub (`SfxKind::Gui`) |
| `7zCon.sfx` | 194048 | Console extractor stub (`SfxKind::Console`) |

## License

7-Zip is distributed under the **GNU LGPL v2.1+** with the unRAR restriction.
The full license text ships alongside the stubs as `LICENSE.txt` (copied from
the distribution). See <https://www.7-zip.org/license.txt>.

These stubs are vendored as distribution material only; they are concatenated
byte-for-byte with a generated archive and are never modified or statically
linked.

## PE architecture

Both stubs were checked via their PE COFF header:

| File | `MZ` | `COFF.Machine` | Meaning |
|------|------|----------------|---------|
| `7z.sfx`   | yes | `0x014C` | IMAGE_FILE_MACHINE_I386 (32-bit x86) |
| `7zCon.sfx`| yes | `0x014C` | IMAGE_FILE_MACHINE_I386 (32-bit x86) |

- The official 26.03 distribution ships **x86 (32-bit)** stubs only; no
  x64-specific or ARM64-native `.sfx` is provided.
- On Windows x64 these run natively; on Windows ARM64 they run under the
  built-in x86 emulation layer. No architecture-specific handling is required
  on our side.

## Runtime resolution

`find_sfx_stub` searches, in order:

1. `%ZIPNEST_SFX_DIR%\<stub>` (explicit override, dev/tests)
2. `<exe-dir>\engines\sfx\<stub>` (deployment layout)
3. `vendor/7zip-sfx/<stub>` (workspace, at test time)

Deployments should ship `engines/sfx/{7z.sfx,7zCon.sfx}` next to the executable.

# vendor/7zip-bin — the shipped 7z.dll

`7z.dll` here is the **unmodified** official 7-Zip engine the app loads at runtime
(LGPL compliance: the project ships the untouched binary and never links it
statically; see `crates/archive-core/src/dll.rs`).

It lives in the repo so that the installer payload can be produced from a clean
checkout. Before v0.4.11 the only copy was inside the gitignored
`dist-portable\`, which meant anyone but the original maintainer could not build
an installer at all (`packaging\installer.nsi` reads every file from there).

| | |
| --- | --- |
| File | `7z.dll` |
| SHA256 | `65E4C1F855F9EF6E8F0F5DF8E3F27D9EB5F07311408639DA0A1CA0B8F4871B0D` |
| Size | 1906688 bytes |
| Architecture | x64 |
| License text shipped as | `licenses\7zip.txt` (from `vendor\7zip-sdk\LICENSE`) |

Provenance: this is the engine the project has shipped since the first release —
taken from the official 7-Zip distribution and copied forward byte for byte (the
hash above also matches the copy under `apps/zipnest/src-tauri/resources`, the
older, unshipped web build).

Replacing it: drop the new `7z.dll` in here, update the hash table above and the
`licenses\7zip.txt` copy, then re-run `packaging\emit-portable.ps1`. Users may
also replace the DLL next to the installed exe themselves — the app probes the
file at runtime and reports the real load error when it does not fit.

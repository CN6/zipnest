# v0.4.24 — Extract and delete

## Goal

Add a destructive `解压并删除` action beside `开始解压` in the extract dialog.
The source archive is removed only after every selected entry has extracted
successfully. A failed, cancelled, or password-refused extraction keeps it.

## Design

- Carry `delete_archive` with the existing `ExtractRequest`, so password retries
  replay exactly the same operation.
- Store each open archive's source path in `ArchiveRegistry`. The IPC extract
  worker owns an `Arc<ArchiveRegistry>` and, after the success checks pass,
  releases the source handle, removes the registry entry, then deletes the file.
- If deletion fails, the job fails with `error.archive_delete`; extracted files
  and the source both remain. The UI closes the dead archive view because the
  handle was already released.
- Add the two-button row and zh-CN/en-US labels. Successful deletion uses a
  distinct completion message.

## Tests

- Successful selected extraction with deletion removes the source, leaves the
  extracted file, and closes the registry entry.
- A failed extraction with deletion requested keeps the source.
- `cargo test --workspace` and warning-free clippy.

## Release

- Bump the workspace and packaging versions to `0.4.24`.
- Add the user-facing CHANGELOG entry and update README version references.
- Build the native release binary and copy it to `D:\ZipNest\zipnest.exe`.

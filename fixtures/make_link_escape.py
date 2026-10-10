"""Builds fixtures/link-escape.tar: a hostile TAR whose first entry is a
directory symlink pointing outside the extraction root, followed by a file that
writes through it.

If the extractor follows the link, `pwn.txt` lands next to the destination
instead of inside it. `crates/archive-core/tests/link_escape.rs` extracts this
and asserts that nothing escaped.
"""
import io
import os
import tarfile

here = os.path.dirname(os.path.abspath(__file__))
out = os.path.join(here, "link-escape.tar")

with tarfile.open(out, "w", format=tarfile.GNU_FORMAT) as tar:
    # 1. `d` is a symlink to the parent of the extraction root.
    link = tarfile.TarInfo("d")
    link.type = tarfile.SYMTYPE
    link.linkname = ".."
    link.mode = 0o777
    tar.addfile(link)

    # 2. written "inside" d, i.e. one level above the destination.
    payload = b"ESCAPED\n"
    entry = tarfile.TarInfo("d/pwn.txt")
    entry.size = len(payload)
    entry.mode = 0o644
    tar.addfile(entry, io.BytesIO(payload))

    # 3. a plain file too, so the test can prove the extraction itself worked.
    plain = b"ok\n"
    keep = tarfile.TarInfo("keep.txt")
    keep.size = len(plain)
    keep.mode = 0o644
    tar.addfile(keep, io.BytesIO(plain))

print("wrote", out, os.path.getsize(out), "bytes")

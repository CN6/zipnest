import { describe, expect, it } from "vitest";
import { classifyEntry, decodeText, extensionOf, formatHexDump } from "./previewCodec";

const bytes = (...values: number[]) => Uint8Array.from(values);
const utf8 = (s: string) => new TextEncoder().encode(s);

describe("classifyEntry", () => {
  it("detects a UTF-8 BOM before anything else", () => {
    expect(classifyEntry("a.txt", bytes(0xef, 0xbb, 0xbf, 0x48, 0x69))).toEqual({
      kind: "text",
      encoding: "utf-8",
    });
  });

  it("detects UTF-16 LE and BE BOMs", () => {
    expect(classifyEntry("a.txt", bytes(0xff, 0xfe, 0x68, 0x00))).toEqual({
      kind: "text",
      encoding: "utf-16le",
    });
    expect(classifyEntry("a.txt", bytes(0xfe, 0xff, 0x00, 0x68))).toEqual({
      kind: "text",
      encoding: "utf-16be",
    });
  });

  it("treats valid UTF-8 (including non-ASCII) as utf-8 text", () => {
    expect(classifyEntry("note.txt", utf8("hello"))).toEqual({
      kind: "text",
      encoding: "utf-8",
    });
    expect(classifyEntry("note.txt", utf8("你好，世界"))).toEqual({
      kind: "text",
      encoding: "utf-8",
    });
    expect(classifyEntry("empty.txt", bytes())).toEqual({
      kind: "text",
      encoding: "utf-8",
    });
  });

  it("falls back to GBK when bytes are invalid UTF-8 but plausible GBK", () => {
    // GBK for "中文" — not a valid UTF-8 byte string.
    expect(classifyEntry("legacy.txt", bytes(0xd6, 0xd0, 0xce, 0xc4))).toEqual({
      kind: "text",
      encoding: "gbk",
    });
  });

  it("classifies images by extension, even with binary bytes", () => {
    // Real PNG signature: invalid UTF-8, yet the leading bytes satisfy the GBK
    // heuristic — extension must win.
    const png = bytes(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a);
    expect(classifyEntry("logo.png", png)).toEqual({ kind: "image" });

    const jpeg = bytes(0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10);
    expect(classifyEntry("photo.JPG", jpeg)).toEqual({ kind: "image" });
    for (const ext of ["jpeg", "gif", "bmp", "webp"]) {
      expect(classifyEntry(`x.${ext}`, png)).toEqual({ kind: "image" });
    }
  });

  it("returns hex for binary that is neither UTF-8 nor plausible GBK", () => {
    // 0xff is a GBK lead, but 0x00 is not a legal trail byte.
    expect(classifyEntry("blob.bin", bytes(0xff, 0x00, 0xff))).toEqual({ kind: "hex" });
    expect(classifyEntry("dangling.bin", bytes(0x81))).toEqual({ kind: "hex" });
  });

  it("keeps an image extension as-is only for known image types", () => {
    expect(classifyEntry("data.dat", utf8("plain"))).toEqual({
      kind: "text",
      encoding: "utf-8",
    });
  });
});

describe("extensionOf", () => {
  it("handles paths, backslashes and dotfiles", () => {
    expect(extensionOf("dir/sub/Photo.PNG")).toBe("png");
    expect(extensionOf("dir\\sub\\a.JPEG")).toBe("jpeg");
    expect(extensionOf(".bashrc")).toBe("");
    expect(extensionOf("noext")).toBe("");
  });
});

describe("decodeText", () => {
  it("decodes UTF-8 and strips a BOM", () => {
    expect(decodeText(bytes(0xef, 0xbb, 0xbf, 0x68, 0x69), "utf-8")).toBe("hi");
  });
});

describe("formatHexDump", () => {
  it("renders the offset, hex column and ascii column", () => {
    const dump = formatHexDump(utf8("AB"), 4096);
    expect(dump).toMatch(/^00000000 {2}41 42 +\|AB\|$/);
  });

  it("limits output to the requested byte count", () => {
    const dump = formatHexDump(Uint8Array.from({ length: 32 }, (_, i) => i), 16);
    expect(dump.split("\n")).toHaveLength(1);
  });
});

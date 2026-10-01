// Pure preview classification + formatting helpers.
//
// `classifyEntry` performs no I/O and depends on no ambient state, so it is
// fully unit-testable. The frontend never reads the archive directly — the
// caller fetches the bytes through IPC (`read_entry_bytes`) and hands them here.

/** What the preview panel should render for one entry. */
export type PreviewKind = "text" | "image" | "hex" | "other";

/** Text encodings the sniff step can report. */
export type TextEncoding = "utf-8" | "utf-16le" | "utf-16be" | "gbk";

export interface PreviewInfo {
  kind: PreviewKind;
  /** Present only when `kind === "text"`. */
  encoding?: TextEncoding;
}

/** Image types the WebView2 renderer handles natively. */
const IMAGE_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "bmp", "webp"]);

const IMAGE_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  bmp: "image/bmp",
  webp: "image/webp",
};

/** Lower-cased extension of the last path segment, without the dot. */
export function extensionOf(name: string): string {
  const base = name.replace(/\\/g, "/").split("/").pop() ?? "";
  const dot = base.lastIndexOf(".");
  return dot > 0 && dot < base.length - 1 ? base.slice(dot + 1).toLowerCase() : "";
}

/** MIME type for a known image entry, else `application/octet-stream`. */
export function imageMime(name: string): string {
  return IMAGE_MIME[extensionOf(name)] ?? "application/octet-stream";
}

/**
 * Strict UTF-8 validator (no overlong forms, no surrogates, max U+10FFFF).
 *
 * Hand-rolled rather than `TextDecoder({ fatal: true })` so the result is
 * identical in the browser, jsdom, and any Node version.
 */
function isValidUtf8(bytes: Uint8Array): boolean {
  const len = bytes.length;
  let i = 0;
  while (i < len) {
    const b = bytes[i];
    if (b <= 0x7f) {
      i += 1;
      continue;
    }
    let need: number;
    let lo = 0x80;
    let hi = 0xbf;
    if (b >= 0xc2 && b <= 0xdf) {
      need = 1;
    } else if (b === 0xe0) {
      need = 2;
      lo = 0xa0;
    } else if (b >= 0xe1 && b <= 0xec) {
      need = 2;
    } else if (b === 0xed) {
      need = 2;
      hi = 0x9f; // exclude UTF-16 surrogates
    } else if (b === 0xee || b === 0xef) {
      need = 2;
    } else if (b === 0xf0) {
      need = 3;
      lo = 0x90;
    } else if (b >= 0xf1 && b <= 0xf3) {
      need = 3;
    } else if (b === 0xf4) {
      need = 3;
      hi = 0x8f; // cap at U+10FFFF
    } else {
      return false;
    }
    if (i + need >= len) return false;
    if (bytes[i + 1] < lo || bytes[i + 1] > hi) return false;
    for (let k = 2; k <= need; k += 1) {
      const c = bytes[i + k];
      if (c < 0x80 || c > 0xbf) return false;
    }
    i += need + 1;
  }
  return true;
}

/**
 * Plausibility check for GBK/CP936, the common legacy Chinese encoding.
 *
 * Only reachable when the bytes are *not* valid UTF-8, so any non-ASCII byte
 * here is already an invalid UTF-8 sequence. GBK maps every lead byte 0x81–0xFE
 * to a trail byte in 0x40–0x7E or 0x80–0xFE (0x7F is reserved). We accept the
 * byte string only if it parses end-to-end under those rules and consumes at
 * least one double-byte character — the same conservative heuristic browsers
 * use when there is no charset declaration.
 */
function looksLikeGbk(bytes: Uint8Array): boolean {
  const len = bytes.length;
  let i = 0;
  let sawMultiByte = false;
  while (i < len) {
    const b = bytes[i];
    if (b <= 0x7f) {
      i += 1;
      continue;
    }
    const trail = i + 1 < len ? bytes[i + 1] : -1;
    const trailOk = (trail >= 0x40 && trail <= 0x7e) || (trail >= 0x80 && trail <= 0xfe);
    if (b >= 0x81 && b <= 0xfe && trailOk) {
      i += 2;
      sawMultiByte = true;
      continue;
    }
    return false;
  }
  return sawMultiByte;
}

/**
 * Classify one entry for preview. Deterministic order:
 *
 *   1. BOM        — an explicit UTF-8 / UTF-16 marker, strongest signal.
 *   2. extension  — image by extension, checked *before* the text heuristics:
 *                   binary image bytes (a PNG starts `89 50 4E 47`) are
 *                   routinely invalid UTF-8 yet parse as plausible GBK, so a
 *                   content sniff would mislabel a real image as text.
 *   3. UTF-8      — strict validation; ASCII-only binaries land here too, but
 *                   that is the accepted trade-off of charset sniffing.
 *   4. GBK        — invalid UTF-8 that parses cleanly as GBK.
 *   5. hex        — everything else is shown as a byte dump.
 *
 * `kind: "other"` is reserved for the caller when an entry has no previewable
 * representation (e.g. the bytes could not be read); the rules below never
 * emit it.
 */
export function classifyEntry(name: string, bytes: Uint8Array): PreviewInfo {
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
    return { kind: "text", encoding: "utf-8" };
  }
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) {
    return { kind: "text", encoding: "utf-16le" };
  }
  if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) {
    return { kind: "text", encoding: "utf-16be" };
  }

  if (IMAGE_EXTENSIONS.has(extensionOf(name))) {
    return { kind: "image" };
  }

  if (isValidUtf8(bytes)) {
    return { kind: "text", encoding: "utf-8" };
  }

  if (looksLikeGbk(bytes)) {
    return { kind: "text", encoding: "gbk" };
  }

  return { kind: "hex" };
}

/**
 * Decode preview bytes to a string. `TextDecoder` strips a leading BOM for us.
 * Falls back to UTF-8 if the runtime cannot build the requested decoder.
 */
export function decodeText(bytes: Uint8Array, encoding: TextEncoding = "utf-8"): string {
  try {
    return new TextDecoder(encoding).decode(bytes);
  } catch {
    return new TextDecoder("utf-8").decode(bytes);
  }
}

/**
 * Classic `offset  hex  |ascii|` dump of the first `limit` bytes (default
 * 4 KiB). Non-printable bytes render as `.`.
 */
export function formatHexDump(bytes: Uint8Array, limit = 4096): string {
  const view = bytes.subarray(0, Math.min(bytes.length, limit));
  const lines: string[] = [];
  for (let off = 0; off < view.length; off += 16) {
    const chunk = view.subarray(off, off + 16);
    const hex = Array.from(chunk, (b) => b.toString(16).padStart(2, "0"))
      .join(" ")
      .padEnd(47);
    const ascii = Array.from(chunk, (b) =>
      b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : ".",
    ).join("");
    lines.push(`${off.toString(16).padStart(8, "0")}  ${hex}  |${ascii}|`);
  }
  return lines.join("\n");
}

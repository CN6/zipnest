import { describe, expect, it } from "vitest";
import { formatBytes, formatEta, percent } from "./format";

describe("progress helpers", () => {
  it("percent clamps to 0..100 and guards zero totals", () => {
    expect(percent(0, 100)).toBe(0);
    expect(percent(50, 100)).toBe(50);
    expect(percent(99, 100)).toBe(99);
    expect(percent(200, 100)).toBe(100);
    expect(percent(1, 0)).toBe(0);
    expect(percent(0, 0)).toBe(0);
  });

  it("eta formats mm:ss and guards unknown", () => {
    expect(formatEta(0)).toBe("--:--");
    expect(formatEta(-5)).toBe("--:--");
    expect(formatEta(65)).toBe("01:05");
    expect(formatEta(600)).toBe("10:00");
  });

  it("formatBytes picks sane units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
  });
});

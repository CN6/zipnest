import { describe, expect, it } from "vitest";
import { setLocale, t } from "./index";
import zh from "./locales/zh-CN.json";
import en from "./locales/en-US.json";

// Keys produced by Rust `ZipnestError::error_key()` / `IpcError` — the
// cross-language contract. Add here whenever a new error key ships.
const RUST_ERROR_KEYS = [
  "error.dll_missing",
  "error.not_an_archive",
  "error.password_required",
  "error.password_incorrect",
  "error.password_unsupported",
  "error.cancelled",
  "error.engine",
  "error.security_blocked",
  "error.quota_exceeded",
  "error.io",
  "error.settings.invalid",
  "error.shell.associate",
  "error.shell.file_menu",
  "error.shell.directory_menu",
  "error.shell.background_menu",
];

describe("i18n", () => {
  it("zh and en have identical key sets", () => {
    expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort());
  });

  it("covers every rust error key in both locales", () => {
    for (const key of RUST_ERROR_KEYS) {
      expect(zh, `zh-CN missing ${key}`).toHaveProperty(key);
      expect(en, `en-US missing ${key}`).toHaveProperty(key);
    }
  });

  it("switches locale at runtime", () => {
    setLocale("en-US");
    expect(t("app.title")).toBe(en["app.title"]);
    setLocale("zh-CN");
    expect(t("app.title")).toBe(zh["app.title"]);
  });

  it("interpolates placeholders and keeps unknown ones", () => {
    setLocale("zh-CN");
    expect(t("browser.selected_count", { count: 3 })).toBe("已选 3 项");
    expect(t("browser.selected_count", {})).toContain("{count}");
  });

  it("returns the key itself for unknown keys", () => {
    expect(t("no.such.key")).toBe("no.such.key");
  });
});

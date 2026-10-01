import zhCN from "./locales/zh-CN.json";
import enUS from "./locales/en-US.json";

export type Locale = "zh-CN" | "en-US";

const packs: Record<Locale, Record<string, string>> = {
  "zh-CN": zhCN,
  "en-US": enUS,
};

let current: Locale = (() => {
  if (typeof localStorage !== "undefined") {
    const saved = localStorage.getItem("zipnest.locale");
    if (saved === "zh-CN" || saved === "en-US") return saved;
  }
  return typeof navigator !== "undefined" && navigator.language.startsWith("zh")
    ? "zh-CN"
    : "en-US";
})();

export function setLocale(locale: Locale): void {
  current = locale;
  try {
    localStorage.setItem("zipnest.locale", locale);
  } catch {
    /* private mode / storage disabled: keep the in-memory locale */
  }
}

export function getLocale(): Locale {
  return current;
}

/** Resolve `key` in the active locale; `{name}` placeholders interpolate. */
export function t(key: string, vars?: Record<string, string | number>): string {
  const raw = packs[current][key] ?? key;
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (whole, name) =>
    name in vars ? String(vars[name]) : whole,
  );
}

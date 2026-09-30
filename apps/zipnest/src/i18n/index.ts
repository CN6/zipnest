import zhCN from "./locales/zh-CN.json";
import enUS from "./locales/en-US.json";

export type Locale = "zh-CN" | "en-US";

const packs: Record<Locale, Record<string, string>> = {
  "zh-CN": zhCN,
  "en-US": enUS,
};

let current: Locale =
  typeof navigator !== "undefined" && navigator.language.startsWith("zh")
    ? "zh-CN"
    : "en-US";

export function setLocale(locale: Locale): void {
  current = locale;
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

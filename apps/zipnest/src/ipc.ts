import { invoke } from "@tauri-apps/api/core";

/** One entry as serialized by `zipnest-ipc::EntryDto`. */
export interface EntryDto {
  path: string;
  name: string;
  is_dir: boolean;
  size: number;
  mtime_ms: number | null;
  encrypted: boolean;
}

export interface OpenArchiveResult {
  id: number;
  encrypted: boolean;
  format: string;
  entries: EntryDto[];
}

/** Wire shape of `IpcError` — always a stable `error.*` key. */
export interface IpcError {
  key: string;
}

/**
 * Wire shape of `zipnest-ipc::CreateRequest`. Field names are snake_case
 * because the Rust struct derives `Deserialize` without a rename (only the
 * top-level command args are camelCase-converted by Tauri).
 */
export interface CreateOptions {
  format: string;
  level: string;
  method: string;
  password: string | null;
  encrypt_names: boolean;
  volume_bytes: number | null;
  sfx: string | null;
}

export interface JobProgressEvent {
  job_id: number;
  done_items: number;
  total_items: number;
  done_bytes: number;
  total_bytes: number;
  speed_bps: number;
  eta_secs: number;
}

export interface JobFinishedEvent {
  job_id: number;
  ok: boolean;
  error_key: string | null;
}

/** Extract any thrown value into a stable error key. */
export function errKey(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "key" in e && typeof (e as IpcError).key === "string") {
    return (e as IpcError).key;
  }
  return "error.engine";
}

export const openArchive = (path: string, password?: string) =>
  invoke<OpenArchiveResult>("open_archive", { path, password: password ?? null });

export const listChildren = (id: number, dir: string) =>
  invoke<EntryDto[]>("list_children", { id, dir });

export const readEntryBytes = (id: number, path: string, maxBytes = 64 * 1024 * 1024) =>
  invoke<number[]>("read_entry_bytes", { id, path, maxBytes });

export const extract = (
  id: number,
  paths: string[],
  dest: string,
  overwrite: boolean,
  password?: string,
) =>
  invoke<number>("extract", { id, paths, dest, overwrite, password: password ?? null });

export const createArchive = (sources: string[], dest: string, options: CreateOptions) =>
  invoke<number>("create_archive", { sources, dest, options });

export const jobCancel = (jobId: number) => invoke<boolean>("job_cancel", { jobId });

export const revealInExplorer = (path: string) =>
  invoke<void>("reveal_in_explorer", { path });

export const openEntry = (id: number, path: string) =>
  invoke<void>("open_entry", { id, path });

/** Wire shape of `zipnest-ipc::Settings` (snake_case, no rename). */
export interface Settings {
  language: string;
  default_extract_dir: string;
  overwrite_policy: string;
  associate: boolean;
  context_menu: boolean;
  preview_max_bytes: number;
}

/** Partial update; omitted fields are left untouched. */
export interface SettingsPatch {
  language?: string;
  default_extract_dir?: string;
  overwrite_policy?: string;
  preview_max_bytes?: number;
}

export const settingsGet = () => invoke<Settings>("settings_get");

export const settingsSet = (patch: SettingsPatch) =>
  invoke<Settings>("settings_set", { patch });

/**
 * Register/unregister the per-user Explorer integration. The Rust side only
 * persists these two flags, so the patch above deliberately omits them.
 * Note the top-level command args are camelCase (Tauri converts them).
 */
/** Outcome of `shell_register`: what actually applied plus one warning key
 * per Explorer integration point the OS denied (e.g. a hardened `*\shell`).
 * `blocked` lists the extensions Windows has already assigned to another
 * program: Windows protects that per-extension choice, so only the user can
 * change it in the system Default apps page. */
export interface ShellRegisterResult {
  associate: boolean;
  context_menu: boolean;
  warnings: string[];
  blocked: string[];
}

export const shellRegister = (associate: boolean, contextMenu: boolean) =>
  invoke<ShellRegisterResult>("shell_register", { associate, contextMenu });

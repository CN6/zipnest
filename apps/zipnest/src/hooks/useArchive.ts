import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  CreateOptions,
  EntryDto,
  JobFinishedEvent,
  JobProgressEvent,
  OpenArchiveResult,
  createArchive as ipcCreateArchive,
  errKey,
  extract as ipcExtract,
  jobCancel,
  listChildren,
  openArchive,
  readEntryBytes,
} from "../ipc";
import { PreviewInfo, classifyEntry } from "../lib/previewCodec";

export type SortKey = "name" | "size" | "mtime";
export interface SortState {
  key: SortKey;
  asc: boolean;
}
export type Status = "closed" | "opening" | "open" | "extracting" | "creating";

export interface JobState {
  jobId: number;
  doneItems: number;
  totalItems: number;
  doneBytes: number;
  totalBytes: number;
  speedBps: number;
  etaSecs: number;
  /** No progress event yet → still queued behind another job. */
  started: boolean;
}

/** Bytes + classification shown by the preview panel for one selection. */
export interface PreviewData {
  entry: EntryDto;
  info: PreviewInfo;
  bytes: Uint8Array;
}

/** Preview read caps — keep a huge entry from ever landing in the webview. */
const PREVIEW_SNIFF_BYTES = 64 * 1024;
const PREVIEW_TEXT_BYTES = 256 * 1024;
const PREVIEW_IMAGE_BYTES = 8 * 1024 * 1024;
const PREVIEW_HEX_BYTES = 4 * 1024;

/** `read_entry_bytes` returns a JSON number array; wrap it for the codec. */
const toBytes = (raw: number[]) => new Uint8Array(raw);

/** What the password dialog is retrying. */
export type PendingPassword =
  | { kind: "open"; path: string }
  | { kind: "extract"; paths: string[]; dest: string; overwrite: boolean };

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function sortRows(rows: EntryDto[], sort: SortState): EntryDto[] {
  const dirFirst = (a: EntryDto, b: EntryDto) =>
    Number(b.is_dir) - Number(a.is_dir) || collator.compare(a.name, b.name);
  const cmp: Record<SortKey, (a: EntryDto, b: EntryDto) => number> = {
    name: dirFirst,
    size: (a, b) => a.size - b.size || dirFirst(a, b),
    mtime: (a, b) => (a.mtime_ms ?? 0) - (b.mtime_ms ?? 0) || dirFirst(a, b),
  };
  const base = [...rows].sort(cmp[sort.key]);
  return sort.asc ? base : base.reverse();
}

/** Normalized path for comparisons (archives may store `sub\b.txt`). */
function norm(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "");
}

export function useArchive() {
  const [status, setStatus] = useState<Status>("closed");
  const [archive, setArchive] = useState<OpenArchiveResult | null>(null);
  const [archivePath, setArchivePath] = useState("");
  const [cwd, setCwd] = useState("");
  const [rawRows, setRawRows] = useState<EntryDto[]>([]);
  const [sort, setSort] = useState<SortState>({ key: "name", asc: true });
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [errorKey, setErrorKey] = useState<string | null>(null);
  /** i18n key shown as a non-error notice (success, canceled …). */
  const [notice, setNotice] = useState<string | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  /** Args the password dialog will retry with (holder, dialog is separate). */
  const [pendingPassword, setPendingPassword] = useState<PendingPassword | null>(null);
  const [passwordDialogOpen, setPasswordDialogOpen] = useState(false);
  /** True after a submitted password was rejected (dialog shows a hint). */
  const [passwordWrong, setPasswordWrong] = useState(false);
  const [extractDest, setExtractDest] = useState("");
  const [createDest, setCreateDest] = useState("");
  const [preview, setPreview] = useState<PreviewData | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  /** Monotonic token: a stale fetch resolving late must not overwrite state. */
  const previewSeqRef = useRef(0);
  const anchorRef = useRef<string | null>(null);
  const jobRef = useRef<number | null>(null);
  /** Which lifecycle started the in-flight job (drives finish handling). */
  const jobKindRef = useRef<"extract" | "create">("extract");
  /** Status to restore once a create job settles (create is archive-agnostic). */
  const restoreStatusRef = useRef<Status>("closed");

  const rows = useMemo(() => sortRows(rawRows, sort), [rawRows, sort]);

  const clearError = useCallback(() => setErrorKey(null), []);
  const clearNotice = useCallback(() => setNotice(null), []);

  const clearPreview = useCallback(() => {
    previewSeqRef.current += 1;
    setPreview(null);
    setPreviewError(null);
    setPreviewLoading(false);
  }, []);

  // Preview is a bounded read through IPC: sniff a small window to classify,
  // then fetch only as many bytes as that kind can usefully render.
  const previewEntry = useCallback(
    async (entry: EntryDto) => {
      if (!archive || entry.is_dir) {
        clearPreview();
        return;
      }
      const seq = ++previewSeqRef.current;
      setPreviewLoading(true);
      setPreviewError(null);
      const stale = () => seq !== previewSeqRef.current;
      try {
        const sniff = toBytes(await readEntryBytes(archive.id, entry.path, PREVIEW_SNIFF_BYTES));
        if (stale()) return;
        const info = classifyEntry(entry.name || entry.path, sniff);
        let bytes = sniff;
        if (info.kind === "image") {
          const cap = Math.max(sniff.length, Math.min(entry.size, PREVIEW_IMAGE_BYTES));
          if (cap > sniff.length) {
            bytes = toBytes(await readEntryBytes(archive.id, entry.path, cap));
          }
        } else if (info.kind === "text") {
          const cap = Math.max(
            sniff.length,
            Math.min(entry.size || sniff.length, PREVIEW_TEXT_BYTES),
          );
          if (cap > sniff.length) {
            bytes = toBytes(await readEntryBytes(archive.id, entry.path, cap));
          }
        } else if (info.kind === "hex") {
          bytes = sniff.subarray(0, PREVIEW_HEX_BYTES);
        }
        if (stale()) return;
        setPreview({ entry, info, bytes });
      } catch (e) {
        if (stale()) return;
        setPreviewError(errKey(e));
        setPreview(null);
      } finally {
        if (!stale()) setPreviewLoading(false);
      }
    },
    [archive, clearPreview],
  );

  const openByPath = useCallback(async (path: string, password?: string) => {
    setStatus("opening");
    setErrorKey(null);
    setPendingPassword(null);
    setPasswordWrong(false);
    try {
      const r = await openArchive(path, password);
      setArchive(r);
      setArchivePath(path);
      setCwd("");
      setRawRows(r.entries);
      setSelected(new Set());
      anchorRef.current = null;
      setStatus("open");
      setPasswordDialogOpen(false);
      setPendingPassword(null);
      setPasswordWrong(false);
      return r;
    } catch (e) {
      const key = errKey(e);
      if (key === "error.password_required") {
        setPendingPassword({ kind: "open", path });
        setPasswordDialogOpen(true);
      } else if (key === "error.password_incorrect") {
        // Wrong password on open: keep the dialog up with inline feedback
        // and restore the pending holder (cleared at entry above).
        setPendingPassword({ kind: "open", path });
        setPasswordDialogOpen(true);
        setPasswordWrong(true);
      } else {
        setErrorKey(key);
      }
      setStatus("closed");
      return null;
    }
  }, []);

  const navigate = useCallback(
    async (dir: string) => {
      if (!archive) return;
      try {
        const kids = await listChildren(archive.id, dir);
        setCwd(dir);
        setRawRows(kids);
        setSelected(new Set());
        anchorRef.current = null;
        setErrorKey(null);
      } catch (e) {
        setErrorKey(errKey(e));
      }
    },
    [archive],
  );

  const navigateUp = useCallback(() => {
    if (!cwd) return;
    const trimmed = norm(cwd);
    const parent = trimmed.includes("/") ? trimmed.slice(0, trimmed.lastIndexOf("/")) : "";
    void navigate(parent);
  }, [cwd, navigate]);

  const toggleSort = useCallback((key: SortKey) => {
    setSort((s) => (s.key === key ? { key, asc: !s.asc } : { key, asc: true }));
  }, []);

  const clickSelect = useCallback(
    (entry: EntryDto, index: number, e: { ctrlKey: boolean; shiftKey: boolean }) => {
      const path = entry.path;
      if (e.shiftKey && anchorRef.current) {
        const from = rows.findIndex((r) => r.path === anchorRef.current);
        if (from >= 0) {
          const [lo, hi] = from <= index ? [from, index] : [index, from];
          setSelected(new Set(rows.slice(lo, hi + 1).map((r) => r.path)));
          return;
        }
      }
      if (e.ctrlKey) {
        setSelected((prev) => {
          const next = new Set(prev);
          if (next.has(path)) next.delete(path);
          else next.add(path);
          return next;
        });
        anchorRef.current = path;
        return;
      }
      anchorRef.current = path;
      setSelected(new Set([path]));
    },
    [rows],
  );

  const selectAll = useCallback(() => setSelected(new Set(rows.map((r) => r.path))), [rows]);
  const clearSelection = useCallback(() => setSelected(new Set()), []);

  const selectedEntries = useMemo(
    () => rows.filter((r) => selected.has(r.path)),
    [rows, selected],
  );

  const parentOf = useCallback((dir: string) => {
    const t = norm(dir);
    return t.includes("/") ? t.slice(0, t.lastIndexOf("/")) : "";
  }, []);

  // ---- extract lifecycle -------------------------------------------------

  // Job events race the invoke() response: a fast job can emit progress (or
  // even finish) before `extract` resolves and jobRef is set. Buffer those
  // early events by job id and replay them the moment jobRef is assigned.
  const earlyEvents = useRef(
    new Map<number, Array<{ kind: "p"; p: JobProgressEvent } | { kind: "f"; f: JobFinishedEvent }>>(),
  );

  const applyProgress = useCallback((p: JobProgressEvent) => {
    setJob((prev) =>
      prev && prev.jobId === p.job_id
        ? {
            jobId: p.job_id,
            doneItems: p.done_items,
            totalItems: p.total_items,
            doneBytes: p.done_bytes,
            totalBytes: p.total_bytes,
            speedBps: p.speed_bps,
            etaSecs: p.eta_secs,
            started: true,
          }
        : prev,
    );
  }, []);

  const applyFinished = useCallback((p: JobFinishedEvent) => {
    const kind = jobKindRef.current;
    jobRef.current = null;
    setJob(null);
    // Extract returns to the (still-open) listing; create leaves whatever
    // archive was open before it untouched.
    setStatus(kind === "create" ? restoreStatusRef.current : "open");
    if (p.ok) {
      setNotice(kind === "create" ? "create.success" : "extract.success");
      setPendingPassword(null);
      setPasswordDialogOpen(false);
      return;
    }
    const key = p.error_key ?? "error.engine";
    if (kind === "create") {
      // Creation failures never carry a retryable password prompt; a password
      // is supplied up front, and TAR+password is blocked in the wizard.
      if (key === "error.cancelled") setNotice("job.canceled");
      else setErrorKey(key);
      return;
    }
    if (key === "error.password_incorrect") {
      // Keep the holder args; open the dialog for a retry.
      setPasswordDialogOpen(true);
      setPasswordWrong(true);
    } else if (key === "error.password_required") {
      // Encrypted entries but no password anywhere: prompt (holder args
      // are still pending from startExtract).
      setPasswordDialogOpen(true);
      setPasswordWrong(false);
    } else if (key === "error.cancelled") {
      setNotice("job.canceled");
    } else {
      setErrorKey(key);
    }
  }, []);

  const startExtract = useCallback(
    async (paths: string[], dest: string, overwrite: boolean, password?: string) => {
      if (!archive || paths.length === 0) return;
      setErrorKey(null);
      setNotice(null);
      setPasswordWrong(false);
      jobKindRef.current = "extract";
      try {
        const jobId = await ipcExtract(archive.id, paths, dest, overwrite, password);
        // Holder for a possible password retry; dialog only opens on failure.
        setPendingPassword({ kind: "extract", paths, dest, overwrite });
        jobRef.current = jobId;
        setJob({
          jobId,
          doneItems: 0,
          totalItems: 0,
          doneBytes: 0,
          totalBytes: 0,
          speedBps: 0,
          etaSecs: 0,
          started: false,
        });
        setExtractDest(dest);
        setStatus("extracting");
        // Replay anything that arrived while the invoke was in flight.
        const early = earlyEvents.current.get(jobId);
        if (early) {
          earlyEvents.current.delete(jobId);
          for (const ev of early) {
            if (jobRef.current !== jobId) break; // finished already
            if (ev.kind === "p") applyProgress(ev.p);
            else applyFinished(ev.f);
          }
        }
      } catch (e) {
        setErrorKey(errKey(e));
      }
    },
    [archive, applyProgress, applyFinished],
  );

  // A create job needs no open archive and leaves any open listing alone; it
  // reuses the same job state machine (progress strip + cancel + finish) that
  // extract drives through `job_progress`/`job_finished`.
  const startCreate = useCallback(
    async (sources: string[], dest: string, options: CreateOptions) => {
      if (sources.length === 0 || !dest.trim()) return;
      setErrorKey(null);
      setNotice(null);
      setPasswordWrong(false);
      jobKindRef.current = "create";
      restoreStatusRef.current = status === "open" ? "open" : "closed";
      try {
        const jobId = await ipcCreateArchive(sources, dest, options);
        jobRef.current = jobId;
        setJob({
          jobId,
          doneItems: 0,
          totalItems: 0,
          doneBytes: 0,
          totalBytes: 0,
          speedBps: 0,
          etaSecs: 0,
          started: false,
        });
        setCreateDest(dest);
        setStatus("creating");
        // Replay anything that arrived while the invoke was in flight.
        const early = earlyEvents.current.get(jobId);
        if (early) {
          earlyEvents.current.delete(jobId);
          for (const ev of early) {
            if (jobRef.current !== jobId) break; // finished already
            if (ev.kind === "p") applyProgress(ev.p);
            else applyFinished(ev.f);
          }
        }
      } catch (e) {
        setErrorKey(errKey(e));
      }
    },
    [status, applyProgress, applyFinished],
  );

  const cancelJob = useCallback(() => {
    if (jobRef.current != null) void jobCancel(jobRef.current);
  }, []);

  const submitPassword = useCallback(
    (password: string) => {
      const pending = pendingPassword;
      if (!pending) return;
      if (pending.kind === "open") {
        void openByPath(pending.path, password);
      } else {
        void startExtract(pending.paths, pending.dest, pending.overwrite, password);
      }
    },
    [pendingPassword, openByPath, startExtract],
  );

  const dismissPassword = useCallback(() => {
    setPasswordDialogOpen(false);
    setPendingPassword(null);
  }, []);

  // Job events arrive on worker threads through the Tauri event bridge.
  useEffect(() => {
    const unsubs: Promise<() => void>[] = [
      listen<JobProgressEvent>("job_progress", (e) => {
        const p = e.payload;
        if (p.job_id === jobRef.current) {
          applyProgress(p);
        } else if (jobRef.current == null) {
          // Invoke response hasn't landed yet — buffer until startExtract
          // assigns jobRef and replays.
          const arr = earlyEvents.current.get(p.job_id) ?? [];
          arr.push({ kind: "p", p });
          earlyEvents.current.set(p.job_id, arr);
        }
      }),
      listen<JobFinishedEvent>("job_finished", (e) => {
        const p = e.payload;
        if (p.job_id === jobRef.current) {
          applyFinished(p);
        } else if (jobRef.current == null) {
          const arr = earlyEvents.current.get(p.job_id) ?? [];
          arr.push({ kind: "f", f: p });
          earlyEvents.current.set(p.job_id, arr);
        }
      }),
    ];
    return () => {
      unsubs.forEach((u) => u.then((fn) => fn()).catch(() => {}));
    };
  }, [applyProgress, applyFinished]);

  return {
    status,
    archive,
    archivePath,
    cwd,
    rows,
    sort,
    selected,
    selectedEntries,
    errorKey,
    notice,
    job,
    pendingPassword,
    passwordDialogOpen,
    passwordWrong,
    extractDest,
    createDest,
    preview,
    previewLoading,
    previewError,
    parentOf,
    openByPath,
    navigate,
    navigateUp,
    toggleSort,
    clickSelect,
    selectAll,
    clearSelection,
    clearError,
    clearNotice,
    setErrorKey,
    startExtract,
    startCreate,
    previewEntry,
    clearPreview,
    cancelJob,
    submitPassword,
    dismissPassword,
    setPasswordDialogOpen,
  };
}

export type ArchiveApi = ReturnType<typeof useArchive>;

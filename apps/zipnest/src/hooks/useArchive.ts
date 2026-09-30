import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  EntryDto,
  JobFinishedEvent,
  JobProgressEvent,
  OpenArchiveResult,
  errKey,
  extract as ipcExtract,
  jobCancel,
  listChildren,
  openArchive,
} from "../ipc";

export type SortKey = "name" | "size" | "mtime";
export interface SortState {
  key: SortKey;
  asc: boolean;
}
export type Status = "closed" | "opening" | "open" | "extracting";

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
  const anchorRef = useRef<string | null>(null);
  const jobRef = useRef<number | null>(null);

  const rows = useMemo(() => sortRows(rawRows, sort), [rawRows, sort]);

  const clearError = useCallback(() => setErrorKey(null), []);
  const clearNotice = useCallback(() => setNotice(null), []);

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
      return r;
    } catch (e) {
      const key = errKey(e);
      if (key === "error.password_required") {
        setPendingPassword({ kind: "open", path });
        setPasswordDialogOpen(true);
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

  const startExtract = useCallback(
    async (paths: string[], dest: string, overwrite: boolean, password?: string) => {
      if (!archive || paths.length === 0) return;
      setErrorKey(null);
      setNotice(null);
      setPasswordWrong(false);
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
      } catch (e) {
        setErrorKey(errKey(e));
      }
    },
    [archive],
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
        if (p.job_id !== jobRef.current) return;
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
      }),
      listen<JobFinishedEvent>("job_finished", (e) => {
        const p = e.payload;
        if (p.job_id !== jobRef.current) return;
        jobRef.current = null;
        setJob(null);
        setStatus("open");
        if (p.ok) {
          setNotice("extract.success");
          setPendingPassword(null);
          setPasswordDialogOpen(false);
          return;
        }
        const key = p.error_key ?? "error.engine";
        if (key === "error.password_incorrect") {
          // Keep the holder args; open the dialog for a retry.
          setPasswordDialogOpen(true);
          setPasswordWrong(true);
        } else if (key === "error.cancelled") {
          setNotice("job.canceled");
        } else {
          setErrorKey(key);
        }
      }),
    ];
    return () => {
      unsubs.forEach((u) => u.then((fn) => fn()).catch(() => {}));
    };
  }, []);

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
    cancelJob,
    submitPassword,
    dismissPassword,
    setPasswordDialogOpen,
  };
}

export type ArchiveApi = ReturnType<typeof useArchive>;

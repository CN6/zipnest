import { useCallback, useMemo, useRef, useState } from "react";
import {
  EntryDto,
  OpenArchiveResult,
  errKey,
  listChildren,
  openArchive,
} from "../ipc";

export type SortKey = "name" | "size" | "mtime";
export interface SortState {
  key: SortKey;
  asc: boolean;
}
export type Status = "closed" | "opening" | "open";

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function sortRows(rows: EntryDto[], sort: SortState): EntryDto[] {
  const dirFirst = (a: EntryDto, b: EntryDto) =>
    Number(b.is_dir) - Number(a.is_dir) || collator.compare(a.name, b.name);
  const bySize = (a: EntryDto, b: EntryDto) => a.size - b.size || dirFirst(a, b);
  const byMtime = (a: EntryDto, b: EntryDto) =>
    (a.mtime_ms ?? 0) - (b.mtime_ms ?? 0) || dirFirst(a, b);
  const cmp: Record<SortKey, (a: EntryDto, b: EntryDto) => number> = {
    name: dirFirst,
    size: bySize,
    mtime: byMtime,
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
  const anchorRef = useRef<string | null>(null);

  const rows = useMemo(() => sortRows(rawRows, sort), [rawRows, sort]);

  const clearError = useCallback(() => setErrorKey(null), []);

  const openByPath = useCallback(async (path: string, password?: string) => {
    setStatus("opening");
    setErrorKey(null);
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
      setErrorKey(errKey(e));
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
    const parent = trimmed.includes("/")
      ? trimmed.slice(0, trimmed.lastIndexOf("/"))
      : "";
    void navigate(parent);
  }, [cwd, navigate]);

  /** Sort toggle: same key flips direction, new key starts ascending. */
  const toggleSort = useCallback((key: SortKey) => {
    setSort((s) => (s.key === key ? { key, asc: !s.asc } : { key, asc: true }));
  }, []);

  /**
   * Click semantics: plain = single select, ctrl = toggle,
   * shift = range over the currently displayed (sorted) rows.
   */
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

  /** Selected entries as they appear in the current listing. */
  const selectedEntries = useMemo(
    () => rows.filter((r) => selected.has(r.path)),
    [rows, selected],
  );

  /** Parent segment of a cwd path (handles `/` and `\`). */
  const parentOf = useCallback((dir: string) => {
    const t = norm(dir);
    return t.includes("/") ? t.slice(0, t.lastIndexOf("/")) : "";
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
    parentOf,
    openByPath,
    navigate,
    navigateUp,
    toggleSort,
    clickSelect,
    selectAll,
    clearSelection,
    clearError,
    setErrorKey,
  };
}

export type ArchiveApi = ReturnType<typeof useArchive>;

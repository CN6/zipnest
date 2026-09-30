import { useEffect, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { EntryDto } from "../ipc";
import { SortKey, SortState } from "../hooks/useArchive";
import { formatBytes, formatMtime } from "../utils/format";
import { t } from "../i18n";

const ROW_H = 30;

interface Props {
  rows: EntryDto[];
  selected: ReadonlySet<string>;
  sort: SortState;
  onSort: (key: SortKey) => void;
  onActivate: (entry: EntryDto) => void;
  onClickRow: (
    entry: EntryDto,
    index: number,
    e: { ctrlKey: boolean; shiftKey: boolean },
  ) => void;
  onContextMenu: (entry: EntryDto, x: number, y: number) => void;
}

function sortIndicator(sort: SortState, key: SortKey): string {
  return sort.key === key ? (sort.asc ? " ▲" : " ▼") : "";
}

/** Virtualized entry list: sort headers, multi-select, dblclick, context menu. */
export default function EntryTable({
  rows,
  selected,
  sort,
  onSort,
  onActivate,
  onClickRow,
  onContextMenu,
}: Props) {
  const parentRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_H,
    overscan: 12,
  });

  useEffect(() => {
    virtualizer.measure();
  }, [rows, virtualizer]);

  return (
    <div className="table-wrap">
      <div className="table-head" role="row">
        <button type="button" className="th th-name" onClick={() => onSort("name")}>
          {t("browser.columns.name")}
          {sortIndicator(sort, "name")}
        </button>
        <button type="button" className="th th-size" onClick={() => onSort("size")}>
          {t("browser.columns.size")}
          {sortIndicator(sort, "size")}
        </button>
        <button type="button" className="th th-mtime" onClick={() => onSort("mtime")}>
          {t("browser.columns.mtime")}
          {sortIndicator(sort, "mtime")}
        </button>
        <span className="th th-enc">{t("browser.columns.encrypted")}</span>
      </div>
      <div className="table-body" ref={parentRef}>
        <div
          className="table-spacer"
          style={{ height: virtualizer.getTotalSize() }}
        >
          {virtualizer.getVirtualItems().map((vi) => {
            const row = rows[vi.index];
            const isSel = selected.has(row.path);
            return (
              <div
                key={row.path}
                className={`row ${isSel ? "row-selected" : ""} ${
                  row.is_dir ? "row-dir" : ""
                }`}
                style={{
                  transform: `translateY(${vi.start}px)`,
                  height: ROW_H,
                }}
                onClick={(e) =>
                  onClickRow(row, vi.index, {
                    ctrlKey: e.ctrlKey || e.metaKey,
                    shiftKey: e.shiftKey,
                  })
                }
                onDoubleClick={() => onActivate(row)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  if (!isSel) onClickRow(row, vi.index, { ctrlKey: false, shiftKey: false });
                  onContextMenu(row, e.clientX, e.clientY);
                }}
              >
                <span className="cell cell-name">
                  <span className={`marker ${row.is_dir ? "marker-dir" : "marker-file"}`} />
                  {row.name}
                  {row.is_dir ? "/" : ""}
                </span>
                <span className="cell cell-size">
                  {row.is_dir ? "" : formatBytes(row.size)}
                </span>
                <span className="cell cell-mtime">{formatMtime(row.mtime_ms)}</span>
                <span className="cell cell-enc">
                  {row.encrypted ? <span className="badge">AES</span> : ""}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

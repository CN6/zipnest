import { useEffect, useState } from "react";
import {
  Button,
  FluentProvider,
  webLightTheme,
} from "@fluentui/react-components";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import "./App.css";

import { getLocale, setLocale, t, Locale } from "./i18n";
import { ArchiveApi, useArchive } from "./hooks/useArchive";
import { openEntry } from "./ipc";
import Toolbar from "./components/Toolbar";
import Breadcrumbs from "./components/Breadcrumbs";
import EntryTable from "./components/EntryTable";

const ARCHIVE_FILTERS = [
  {
    name: "Archives",
    extensions: ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"],
  },
];

function activate(arch: ArchiveApi, path: string, isDir: boolean) {
  if (isDir) {
    void arch.navigate(path);
    return;
  }
  if (arch.archive) {
    openEntry(arch.archive.id, path).catch((e) =>
      arch.setErrorKey(typeof e === "string" ? e : "error.engine"),
    );
  }
}

export default function App() {
  const arch = useArchive();
  const [locale, setLocaleState] = useState<Locale>(getLocale());

  const toggleLocale = () => {
    const next: Locale = locale === "zh-CN" ? "en-US" : "zh-CN";
    setLocale(next);
    setLocaleState(next);
  };

  const pickArchive = async () => {
    const path = await open({ multiple: false, filters: ARCHIVE_FILTERS });
    if (typeof path === "string") await arch.openByPath(path);
  };

  // Drag & drop: Tauri intercepts HTML5 drops, so use the webview event.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "drop" && event.payload.paths.length > 0) {
          void arch.openByPath(event.payload.paths[0]);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        /* running outside tauri (plain vite dev) — drop simply won't work */
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Ctrl+A selects the whole listing.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "a" && arch.status === "open") {
        e.preventDefault();
        arch.selectAll();
      }
      if (e.key === "Escape") arch.clearSelection();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [arch]);

  useEffect(() => {
    document.title = t("app.title");
  }, [locale]);

  const showEmpty = arch.status === "closed" || arch.rows.length === 0;

  return (
    <FluentProvider theme={webLightTheme}>
      <div className="app">
        <Toolbar
          locale={locale}
          onToggleLocale={toggleLocale}
          onOpen={() => void pickArchive()}
          archivePath={arch.archivePath}
          selectedCount={arch.selected.size}
          disabled={arch.status === "opening"}
        />
        {arch.status === "open" && (
          <Breadcrumbs cwd={arch.cwd} onNavigate={(dir) => void arch.navigate(dir)} />
        )}
        {arch.status === "open" && arch.rows.length > 0 && (
          <EntryTable
            rows={arch.rows}
            selected={arch.selected}
            sort={arch.sort}
            onSort={arch.toggleSort}
            onActivate={(entry) => activate(arch, entry.path, entry.is_dir)}
            onClickRow={(entry, index, e) => arch.clickSelect(entry, index, e)}
            onContextMenu={() => {
              /* context menu lands in Task 9 */
            }}
          />
        )}
        {showEmpty && (
          <div className="empty">
            <p className="empty-title">
              {arch.status === "opening" ? "…" : t("browser.empty")}
            </p>
            <Button appearance="secondary" onClick={() => void pickArchive()}>
              {t("app.open")}
            </Button>
            <p className="empty-hint">{t("app.open_hint")}</p>
          </div>
        )}
        {arch.errorKey && (
          <div className="errorbar" role="alert">
            <span>{t(arch.errorKey)}</span>
            <Button
              appearance="subtle"
              size="small"
              onClick={arch.clearError}
              aria-label="dismiss"
            >
              ✕
            </Button>
          </div>
        )}
        <footer className="statusbar">
          {arch.status === "open" ? `${arch.rows.length}` : ""}
        </footer>
      </div>
    </FluentProvider>
  );
}

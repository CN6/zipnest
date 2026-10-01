import { useEffect, useRef, useState } from "react";
import {
  Button,
  FluentProvider,
  webLightTheme,
} from "@fluentui/react-components";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

import { getLocale, setLocale, t, Locale } from "./i18n";
import { ArchiveApi, useArchive } from "./hooks/useArchive";
import { CreateOptions, EntryDto, openEntry, revealInExplorer, settingsGet } from "./ipc";
import Toolbar from "./components/Toolbar";
import Breadcrumbs from "./components/Breadcrumbs";
import EntryTable from "./components/EntryTable";
import ExtractDialog from "./components/ExtractDialog";
import CreateWizard from "./components/CreateWizard";
import SettingsDialog from "./components/SettingsDialog";
import DonateDialog from "./components/DonateDialog";
import PasswordDialog from "./components/PasswordDialog";
import ProgressBarStrip from "./components/ProgressBar";
import PreviewPanel from "./components/PreviewPanel";

const ARCHIVE_FILTERS = [
  {
    name: "Archives",
    extensions: ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"],
  },
];

function activate(arch: ArchiveApi, path: string, isDir: boolean) {
  // The extract job holds the archive mutex; navigating would just block.
  if (arch.status === "extracting" || arch.status === "creating") return;
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
  const [extractOpen, setExtractOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [donateOpen, setDonateOpen] = useState(false);
  // Context menu: the entry it targets plus viewport position.
  const [ctxMenu, setCtxMenu] = useState<{
    entry: { path: string; is_dir: boolean };
    x: number;
    y: number;
  } | null>(null);
  const ctxRef = useRef<HTMLDivElement>(null);

  // Close the menu on any outside click.
  useEffect(() => {
    if (!ctxMenu) return;
    const close = (e: MouseEvent) => {
      if (!ctxRef.current?.contains(e.target as Node)) setCtxMenu(null);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [ctxMenu]);

  const toggleLocale = () => {
    const next: Locale = locale === "zh-CN" ? "en-US" : "zh-CN";
    setLocale(next);
    setLocaleState(next);
  };

  // "system" resolves from the OS at startup; explicit choices win.
  const applyLanguage = (language: string) => {
    const resolved: Locale =
      language === "en-US"
        ? "en-US"
        : language === "zh-CN"
          ? "zh-CN"
          : typeof navigator !== "undefined" && navigator.language.startsWith("zh")
            ? "zh-CN"
            : "en-US";
    setLocale(resolved);
    setLocaleState(resolved);
  };

  // Start in the persisted language setting (the launcher may not have set it).
  useEffect(() => {
    void settingsGet()
      .then((s) => applyLanguage(s.language))
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const pickArchive = async () => {
    const path = await open({ multiple: false, filters: ARCHIVE_FILTERS });
    if (typeof path === "string") await arch.openByPath(path);
  };

  const startExtract = (dest: string, overwrite: boolean) => {
    setExtractOpen(false);
    const paths = arch.selectedEntries.map((e) => e.path);
    void arch.startExtract(paths, dest, overwrite);
  };

  const startCreate = (sources: string[], dest: string, options: CreateOptions) => {
    setCreateOpen(false);
    void arch.startCreate(sources, dest, options);
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

  // Open an archive this process was launched with (associated file / CLI),
  // and keep handling `open_file_request` from a second instance (tray keeps
  // the process alive, so launches arrive as events after the first one).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void invoke<string | null>("launch_file")
      .then((path) => {
        if (!cancelled && path) void arch.openByPath(path);
      })
      .catch(() => {
        /* plain vite dev has no command */
      });
    listen<{ path: string }>("open_file_request", (e) => {
      if (e.payload?.path) void arch.openByPath(e.payload.path);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});
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

  // Preview target: exactly one selected row that is not a directory.
  const selectedOne =
    arch.selectedEntries.length === 1 && !arch.selectedEntries[0].is_dir
      ? arch.selectedEntries[0]
      : null;
  const selectedOnePath = selectedOne?.path ?? null;
  const archiveId = arch.archive?.id ?? null;

  useEffect(() => {
    if (!selectedOnePath) {
      arch.clearPreview();
      return;
    }
    const entry = arch.rows.find((r) => r.path === selectedOnePath);
    if (entry) void arch.previewEntry(entry);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedOnePath, archiveId]);

  const openExternal = (entry: EntryDto) => {
    if (arch.archive) {
      openEntry(arch.archive.id, entry.path).catch((e) =>
        arch.setErrorKey(typeof e === "string" ? e : "error.engine"),
      );
    }
  };

  const showEmpty = arch.status === "closed" || arch.rows.length === 0;
  const noticeText = arch.notice ? t(arch.notice) : "";
  const busy = arch.status === "opening" || arch.status === "extracting" || arch.status === "creating";
  const destToReveal =
    arch.notice === "create.success"
      ? arch.createDest
      : arch.notice === "extract.success"
        ? arch.extractDest
        : "";

  return (
    <FluentProvider theme={webLightTheme}>
      <div className="app">
        <Toolbar
          locale={locale}
          onToggleLocale={toggleLocale}
          onOpen={() => void pickArchive()}
          onNew={() => setCreateOpen(true)}
          onExtract={() => setExtractOpen(true)}
          archivePath={arch.archivePath}
          selectedCount={arch.selected.size}
          extractEnabled={arch.status === "open" && arch.selected.size > 0}
          onSettings={() => setSettingsOpen(true)}
          disabled={busy}
        />
        {arch.status !== "closed" && (
          <Breadcrumbs
            cwd={arch.cwd}
            onNavigate={(dir) => (busy ? undefined : void arch.navigate(dir))}
          />
        )}
        {arch.status !== "closed" && arch.rows.length > 0 && (
          <div className="browser-body">
            <EntryTable
              rows={arch.rows}
              selected={arch.selected}
              sort={arch.sort}
              onSort={arch.toggleSort}
              onActivate={(entry) => activate(arch, entry.path, entry.is_dir)}
              onClickRow={(entry, index, e) => arch.clickSelect(entry, index, e)}
              onContextMenu={(entry, x, y) => {
                if (busy) return;
                setCtxMenu({ entry: { path: entry.path, is_dir: entry.is_dir }, x, y });
              }}
            />
            {selectedOne && (
              <PreviewPanel
                entry={selectedOne}
                preview={arch.preview}
                loading={arch.previewLoading}
                errorKey={arch.previewError}
                onOpenExternal={() => openExternal(selectedOne)}
              />
            )}
          </div>
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
        {arch.job && <ProgressBarStrip job={arch.job} onCancel={arch.cancelJob} />}
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
          <span>
            {noticeText || (arch.status !== "closed" ? `${arch.rows.length}` : "")}
          </span>
          {destToReveal && (
            <Button
              appearance="subtle"
              size="small"
              onClick={() => void revealInExplorer(destToReveal).catch(() => {})}
            >
              {t("reveal")}
            </Button>
          )}
          {arch.notice && (
            <Button
              appearance="subtle"
              size="small"
              onClick={arch.clearNotice}
              aria-label="dismiss"
            >
              ✕
            </Button>
          )}
          <span className="toolbar-spacer" />
          <Button appearance="subtle" size="small" onClick={() => setDonateOpen(true)}>
            {t("settings.donate")}
          </Button>
        </footer>
        <ExtractDialog
          open={extractOpen}
          onClose={() => setExtractOpen(false)}
          onStart={startExtract}
          defaultDest={arch.archivePath ? arch.archivePath.replace(/[\\/][^\\/]*$/, "") : ""}
        />
        <CreateWizard
          open={createOpen}
          onClose={() => setCreateOpen(false)}
          onStart={startCreate}
        />
        <SettingsDialog
          open={settingsOpen}
          onClose={() => setSettingsOpen(false)}
          onLanguageChange={applyLanguage}
          onDonate={() => setDonateOpen(true)}
        />
        <DonateDialog open={donateOpen} onClose={() => setDonateOpen(false)} />
        <PasswordDialog
          open={arch.passwordDialogOpen}
          wrong={arch.passwordWrong}
          onCancel={arch.dismissPassword}
          onSubmit={arch.submitPassword}
        />
        {ctxMenu && (
          <div
            ref={ctxRef}
            className="ctx-menu"
            style={{ left: ctxMenu.x, top: ctxMenu.y }}
            role="menu"
          >
            {ctxMenu.entry.is_dir ? (
              <button
                type="button"
                className="ctx-item"
                role="menuitem"
                onClick={() => {
                  void arch.navigate(ctxMenu.entry.path);
                  setCtxMenu(null);
                }}
              >
                {t("browser.columns.name")} →
              </button>
            ) : (
              <button
                type="button"
                className="ctx-item"
                role="menuitem"
                onClick={() => {
                  if (arch.archive) {
                    openEntry(arch.archive.id, ctxMenu.entry.path).catch((e) =>
                      arch.setErrorKey(typeof e === "string" ? e : "error.engine"),
                    );
                  }
                  setCtxMenu(null);
                }}
              >
                {t("open_entry")}
              </button>
            )}
            <button
              type="button"
              className="ctx-item"
              role="menuitem"
              onClick={() => {
                // Extract the selection; if the target isn't in it, select it alone.
                if (!arch.selected.has(ctxMenu.entry.path)) {
                  const idx = arch.rows.findIndex((r) => r.path === ctxMenu.entry.path);
                  if (idx >= 0) {
                    arch.clickSelect(arch.rows[idx], idx, {
                      ctrlKey: false,
                      shiftKey: false,
                    });
                  }
                }
                setExtractOpen(true);
                setCtxMenu(null);
              }}
            >
              {t("extract.start")}…
            </button>
            {/* Explorer reveal is intentionally absent: archive entries are
                not on disk — the success notice offers "Show in Explorer"
                for the extraction destination instead (M2 decision). */}
          </div>
        )}
      </div>
    </FluentProvider>
  );
}

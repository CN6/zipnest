import { useEffect, useState } from "react";
import { Button } from "@fluentui/react-components";
import { EntryDto } from "../ipc";
import { PreviewData } from "../hooks/useArchive";
import { decodeText, formatHexDump, imageMime } from "../lib/previewCodec";
import { t } from "../i18n";

/** Matches the hex cap requested by `previewEntry`. */
const HEX_LIMIT = 4 * 1024;

interface Props {
  entry: EntryDto;
  /** Null while loading, on error, or when nothing previewable is selected. */
  preview: PreviewData | null;
  loading: boolean;
  errorKey: string | null;
  onOpenExternal: () => void;
}

/** Right-hand inspector for exactly one selected file entry. */
export default function PreviewPanel({
  entry,
  preview,
  loading,
  errorKey,
  onOpenExternal,
}: Props) {
  const [url, setUrl] = useState<string | null>(null);

  // Object URLs must be revoked when the bytes change or the panel unmounts.
  useEffect(() => {
    if (!preview || preview.info.kind !== "image") {
      setUrl(null);
      return;
    }
    // Copy into a plain ArrayBuffer-backed view so the BlobPart type holds.
    const copy = new Uint8Array(preview.bytes.length);
    copy.set(preview.bytes);
    const objectUrl = URL.createObjectURL(
      new Blob([copy.buffer], { type: imageMime(entry.name) }),
    );
    setUrl(objectUrl);
    return () => {
      URL.revokeObjectURL(objectUrl);
      setUrl(null);
    };
  }, [preview, entry.name]);

  const ready = !loading && !errorKey && preview !== null;

  return (
    <aside className="preview" aria-label={t("preview.title")}>
      <div className="preview-header">
        <span className="preview-name" title={entry.path}>
          {entry.name}
        </span>
        <Button size="small" appearance="subtle" onClick={onOpenExternal}>
          {t("preview.open_external")}
        </Button>
      </div>
      <div className="preview-body">
        {loading && <p className="preview-hint">{t("preview.loading")}</p>}
        {!loading && errorKey && <p className="preview-hint preview-error">{t(errorKey)}</p>}
        {!loading && !errorKey && !preview && (
          <p className="preview-hint">{t("preview.no_preview")}</p>
        )}
        {ready && preview.info.kind === "image" && url && (
          <img className="preview-img" src={url} alt={entry.name} />
        )}
        {ready && preview.info.kind === "text" && (
          <pre className="preview-pre">{decodeText(preview.bytes, preview.info.encoding)}</pre>
        )}
        {ready && preview.info.kind === "hex" && (
          <>
            <pre className="preview-hex">{formatHexDump(preview.bytes, HEX_LIMIT)}</pre>
            {preview.bytes.length > HEX_LIMIT && (
              <p className="preview-hint">{t("preview.truncated", { bytes: HEX_LIMIT })}</p>
            )}
          </>
        )}
        {ready && preview.info.kind === "other" && (
          <div className="preview-other">
            <p className="preview-hint">{t("preview.no_preview")}</p>
            <Button appearance="primary" onClick={onOpenExternal}>
              {t("preview.open_external")}
            </Button>
          </div>
        )}
      </div>
    </aside>
  );
}

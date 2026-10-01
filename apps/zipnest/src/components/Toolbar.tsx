import { Button } from "@fluentui/react-components";
import { t } from "../i18n";
import { Locale } from "../i18n";

interface Props {
  locale: Locale;
  onToggleLocale: () => void;
  onOpen: () => void;
  onNew: () => void;
  onExtract: () => void;
  archivePath: string;
  selectedCount: number;
  extractEnabled: boolean;
  disabled: boolean;
}

/** Top bar: open archive, new archive, extract selection, language, selection. */
export default function Toolbar({
  locale,
  onToggleLocale,
  onOpen,
  onNew,
  onExtract,
  archivePath,
  selectedCount,
  extractEnabled,
  disabled,
}: Props) {
  return (
    <header className="toolbar">
      <Button appearance="primary" onClick={onOpen} disabled={disabled}>
        {t("app.open")}
      </Button>
      <Button appearance="secondary" onClick={onNew} disabled={disabled}>
        {t("create.new")}
      </Button>
      <Button appearance="secondary" onClick={onExtract} disabled={!extractEnabled || disabled}>
        {t("extract.start")}
      </Button>
      <span className="toolbar-archive" title={archivePath}>
        {archivePath ? archivePath.split(/[\\/]/).pop() : t("browser.empty")}
      </span>
      <span className="toolbar-spacer" />
      {selectedCount > 0 && (
        <span className="toolbar-sel">{t("browser.selected_count", { count: selectedCount })}</span>
      )}
      <Button appearance="subtle" size="small" onClick={onToggleLocale}>
        {locale === "zh-CN" ? "EN" : "中文"}
      </Button>
    </header>
  );
}

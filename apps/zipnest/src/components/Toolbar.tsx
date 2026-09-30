import { Button } from "@fluentui/react-components";
import { t } from "../i18n";
import { Locale } from "../i18n";

interface Props {
  locale: Locale;
  onToggleLocale: () => void;
  onOpen: () => void;
  archivePath: string;
  selectedCount: number;
  disabled: boolean;
}

/** Top bar: open archive, language toggle, selection info. */
export default function Toolbar({
  locale,
  onToggleLocale,
  onOpen,
  archivePath,
  selectedCount,
  disabled,
}: Props) {
  return (
    <header className="toolbar">
      <Button appearance="primary" onClick={onOpen} disabled={disabled}>
        {t("app.open")}
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

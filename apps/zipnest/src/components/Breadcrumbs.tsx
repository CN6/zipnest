import { t } from "../i18n";

interface Props {
  cwd: string;
  onNavigate: (dir: string) => void;
}

function segments(cwd: string): { label: string; path: string }[] {
  if (!cwd) return [];
  const parts = cwd.split(/[/\\]+/).filter(Boolean);
  let acc = "";
  return parts.map((p) => {
    acc = acc ? `${acc}/${p}` : p;
    return { label: p, path: acc };
  });
}

/** `root / sub / deeper` — every segment returns to that directory. */
export default function Breadcrumbs({ cwd, onNavigate }: Props) {
  const segs = segments(cwd);
  return (
    <nav className="breadcrumbs" aria-label="path">
      <button type="button" className="crumb" onClick={() => onNavigate("")}>
        {t("app.title")}
      </button>
      {segs.map((s, i) => (
        <span key={s.path}>
          <span className="crumb-sep">/</span>
          <button
            type="button"
            className={`crumb ${i === segs.length - 1 ? "crumb-current" : ""}`}
            onClick={() => onNavigate(s.path)}
          >
            {s.label}
          </button>
        </span>
      ))}
    </nav>
  );
}

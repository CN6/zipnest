import { Button, ProgressBar } from "@fluentui/react-components";
import { JobState } from "../hooks/useArchive";
import { formatBytes, formatEta, percent } from "../utils/format";
import { t } from "../i18n";

interface Props {
  job: JobState;
  onCancel: () => void;
}

/** Bottom progress strip: bar + speed/ETA + queue hint + cancel. */
export default function ProgressBarStrip({ job, onCancel }: Props) {
  const pct = percent(job.doneBytes, job.totalBytes);
  const queued = !job.started;
  return (
    <div className="progress-strip">
      <div className="progress-main">
        <ProgressBar
          className="progress-bar"
          value={queued ? undefined : pct / 100}
          aria-label="progress"
        />
        <span className="progress-text">
          {queued
            ? t("progress.waiting_queue")
            : `${pct}% · ${t("progress.speed", { speed: formatBytes(job.speedBps) })} · ${t(
                "progress.eta",
                { eta: formatEta(job.etaSecs) },
              )}`}
        </span>
      </div>
      <Button size="small" appearance="secondary" onClick={onCancel}>
        {t("extract.cancel")}
      </Button>
    </div>
  );
}

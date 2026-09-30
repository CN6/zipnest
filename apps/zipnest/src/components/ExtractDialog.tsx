import { useEffect, useState } from "react";
import {
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  Field,
} from "@fluentui/react-components";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { t } from "../i18n";

interface Props {
  open: boolean;
  onClose: () => void;
  onStart: (dest: string, overwrite: boolean) => void;
}

/** Destination picker + overwrite flag for an extraction. */
export default function ExtractDialog({ open, onClose, onStart }: Props) {
  const [dest, setDest] = useState("");
  const [overwrite, setOverwrite] = useState(true);
  const [touched, setTouched] = useState(false);

  useEffect(() => {
    if (open) {
      setDest("");
      setOverwrite(true);
      setTouched(false);
    }
  }, [open]);

  const browse = async () => {
    const dir = await openDialog({ directory: true, multiple: false });
    if (typeof dir === "string") setDest(dir);
  };

  const start = () => {
    setTouched(true);
    if (!dest.trim()) return;
    onStart(dest, overwrite);
  };

  return (
    <Dialog modalType="modal" open={open} onOpenChange={(_, d) => !d && onClose()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("extract.title")}</DialogTitle>
          <DialogContent>
            <Field
              label={t("extract.dest")}
              validationMessage={touched && !dest.trim() ? t("extract.pick_dest") : ""}
              validationState={touched && !dest.trim() ? "error" : "none"}
            >
              <Input value={dest} onChange={(_, d) => setDest(d.value)} />
            </Field>
            <Button onClick={() => void browse()} style={{ marginTop: 8 }}>
              {t("extract.browse")}
            </Button>
            <Checkbox
              checked={overwrite}
              onChange={(_, c) => setOverwrite(Boolean(c.checked))}
              label={t("extract.overwrite")}
              style={{ marginTop: 12, display: "block" }}
            />
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose}>
              {t("extract.cancel")}
            </Button>
            <Button appearance="primary" onClick={start}>
              {t("extract.start")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

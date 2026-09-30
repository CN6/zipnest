import { useEffect, useState } from "react";
import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  Field,
} from "@fluentui/react-components";
import { t } from "../i18n";

interface Props {
  open: boolean;
  wrong: boolean;
  onCancel: () => void;
  onSubmit: (password: string) => void;
}

/** Password prompt for opening or re-extracting an encrypted archive. */
export default function PasswordDialog({ open, wrong, onCancel, onSubmit }: Props) {
  const [password, setPassword] = useState("");

  useEffect(() => {
    if (open) setPassword("");
  }, [open]);

  const submit = () => {
    if (!password) return;
    onSubmit(password);
  };

  return (
    <Dialog modalType="modal" open={open} onOpenChange={(_, d) => !d && onCancel()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("password.title")}</DialogTitle>
          <DialogContent>
            <p style={{ marginTop: 0 }}>{t("password.prompt")}</p>
            <Field
              label={t("password.title")}
              validationMessage={wrong ? t("password.wrong") : ""}
              validationState={wrong ? "error" : "none"}
            >
              <Input
                type="password"
                value={password}
                onChange={(_, d) => setPassword(d.value)}
                onKeyDown={(e) => e.key === "Enter" && submit()}
                autoFocus
              />
            </Field>
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onCancel}>
              {t("password.cancel")}
            </Button>
            <Button appearance="primary" onClick={submit} disabled={!password}>
              {t("password.submit")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

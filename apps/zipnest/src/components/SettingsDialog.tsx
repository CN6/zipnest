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
  Dropdown,
  Field,
  Input,
  Option,
} from "@fluentui/react-components";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { t } from "../i18n";
import { errKey, Settings, settingsGet, settingsSet, shellRegister } from "../ipc";

/** Map persisted language values to their i18n keys (the locale files use
 * `settings.language.zh` / `.en`, not the raw "zh-CN" / "en-US" values). */
const LANGUAGE_KEY: Record<string, string> = {
  system: "settings.language.system",
  "zh-CN": "settings.language.zh",
  "en-US": "settings.language.en",
};

interface Props {
  open: boolean;
  onClose: () => void;
  onLanguageChange: (language: string) => void;
  onDonate: () => void;
}

/** Settings: language, default extract dir, overwrite policy, preview cap,
 * and the HKCU Explorer integration toggles backed by `shell_register`. */
export default function SettingsDialog({ open, onClose, onLanguageChange, onDonate }: Props) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!open) return;
    setError("");
    void settingsGet()
      .then(setSettings)
      .catch((e) => setError(errKey(e)));
  }, [open]);

  if (!settings) {
    // First load (or a load error) — show the shell instead of dereferencing.
    return (
      <Dialog modalType="modal" open={open} onOpenChange={(_, d) => !d && onClose()}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>{t("settings.title")}</DialogTitle>
            <DialogContent>
              {error ? <span role="alert">{t(error)}</span> : t("preview.loading")}
            </DialogContent>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    );
  }

  const patch = (p: Partial<Settings>) => setSettings((s) => (s ? { ...s, ...p } : s));

  const browse = async () => {
    const dir = await openDialog({ directory: true, multiple: false });
    if (typeof dir === "string") patch({ default_extract_dir: dir });
  };

  const setPreviewMb = (value: string) => {
    const n = Number(value);
    if (Number.isFinite(n) && n > 0) {
      patch({ preview_max_bytes: Math.round(n * 1024 * 1024) });
    }
  };

  const save = async () => {
    if (!settings) return;
    setSaving(true);
    setError("");
    try {
      await settingsSet({
        language: settings.language,
        default_extract_dir: settings.default_extract_dir,
        overwrite_policy: settings.overwrite_policy,
        preview_max_bytes: settings.preview_max_bytes,
      });
      const shell = await shellRegister(settings.associate, settings.context_menu);
      // Reflect what actually applied: an OS that denies `*\shell` leaves that
      // toggle off so the UI never lies about the current state.
      setSettings((s) =>
        s ? { ...s, associate: shell.associate, context_menu: shell.context_menu } : s,
      );
      if (shell.warnings.length > 0) {
        setError(shell.warnings[0]);
        // Apply the language even when a shell toggle was denied, so the user
        // is not stuck half-migrated because of an unrelated OS policy.
        onLanguageChange(settings.language);
        return; // stay open with the warning; the denied toggles already reverted
      }
      onLanguageChange(settings.language);
      onClose();
    } catch (e) {
      setError(errKey(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog modalType="modal" open={open} onOpenChange={(_, d) => !d && onClose()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("settings.title")}</DialogTitle>
          <DialogContent>
            <Field label={t("settings.language")}>
              <Dropdown
                value={t(LANGUAGE_KEY[settings.language] ?? settings.language)}
                selectedOptions={[settings.language]}
                onOptionSelect={(_, d) => patch({ language: d.optionValue ?? settings.language })}
              >
                <Option value="system">{t("settings.language.system")}</Option>
                <Option value="zh-CN">{t("settings.language.zh")}</Option>
                <Option value="en-US">{t("settings.language.en")}</Option>
              </Dropdown>
            </Field>

            <Field label={t("settings.default_dir")} style={{ marginTop: 12 }}>
              <Input
                value={settings.default_extract_dir}
                onChange={(_, d) => patch({ default_extract_dir: d.value })}
              />
            </Field>
            <Button onClick={() => void browse()} style={{ marginTop: 8 }}>
              {t("extract.browse")}
            </Button>

            <Field label={t("settings.overwrite")} style={{ marginTop: 12 }}>
              <Dropdown
                value={t(`settings.overwrite.${settings.overwrite_policy}`)}
                selectedOptions={[settings.overwrite_policy]}
                onOptionSelect={(_, d) =>
                  patch({ overwrite_policy: d.optionValue ?? settings.overwrite_policy })
                }
              >
                <Option value="ask">{t("settings.overwrite.ask")}</Option>
                <Option value="overwrite">{t("settings.overwrite.overwrite")}</Option>
                <Option value="skip">{t("settings.overwrite.skip")}</Option>
                <Option value="rename">{t("settings.overwrite.rename")}</Option>
              </Dropdown>
            </Field>

            <Field label={t("settings.preview_limit")} style={{ marginTop: 12 }}>
              <Input
                type="number"
                min={1}
                value={String(settings.preview_max_bytes / (1024 * 1024))}
                onChange={(_, d) => setPreviewMb(d.value)}
              />
            </Field>

            <fieldset
              style={{
                margin: "16px 0 0",
                padding: "10px 12px 4px",
                border: "1px solid #e0e0e0",
                borderRadius: 6,
              }}
            >
              <legend>{t("settings.shell")}</legend>
              <Checkbox
                checked={settings.associate}
                onChange={(_, c) => patch({ associate: Boolean(c.checked) })}
                label={t("settings.shell.associate")}
                style={{ display: "block" }}
              />
              <Checkbox
                checked={settings.context_menu}
                onChange={(_, c) => patch({ context_menu: Boolean(c.checked) })}
                label={t("settings.shell.context_menu")}
                style={{ display: "block", marginTop: 8 }}
              />
            </fieldset>

            {error && (
              <div role="alert" style={{ color: "#c42b1c", marginTop: 12 }}>
                {t(error)}
              </div>
            )}

            <div
              style={{
                marginTop: 16,
                borderTop: "1px solid #eee",
                paddingTop: 8,
                textAlign: "right",
              }}
            >
              <Button appearance="subtle" size="small" onClick={onDonate}>
                {t("settings.donate")}
              </Button>
            </div>
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose} disabled={saving}>
              {t("extract.cancel")}
            </Button>
            <Button appearance="primary" onClick={() => void save()} disabled={saving || !settings}>
              {t("settings.save")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

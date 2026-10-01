import { useEffect, useState, type CSSProperties } from "react";
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
  Radio,
  RadioGroup,
} from "@fluentui/react-components";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { CreateOptions } from "../ipc";
import { t } from "../i18n";

interface Props {
  open: boolean;
  onClose: () => void;
  onStart: (sources: string[], dest: string, options: CreateOptions) => void;
}

const FORMATS = [
  { value: "zip", key: "create.format.zip" },
  { value: "7z", key: "create.format.7z" },
  { value: "tar", key: "create.format.tar" },
  { value: "tar.gz", key: "create.format.targz" },
  { value: "tar.bz2", key: "create.format.tarbz2" },
  { value: "tar.xz", key: "create.format.tarxz" },
] as const;

const FORMAT_KEY: Record<string, string> = Object.fromEntries(
  FORMATS.map((f) => [f.value, f.key]),
);

const LEVELS = ["store", "fastest", "normal", "maximum", "ultra"] as const;
const METHODS = ["auto", "copy", "deflate", "lzma2", "bzip2"] as const;
const VOLUMES = ["off", "10m", "100m", "1g", "custom"] as const;

const MULT: Record<string, number> = { "": 1, k: 1024, m: 1024 ** 2, g: 1024 ** 3, t: 1024 ** 4 };

/** Parse `100m` / `1g` / a raw byte count into bytes; null when invalid. */
export function parseVolume(input: string): number | null {
  const s = input.trim().toLowerCase();
  if (!s) return null;
  const m = s.match(/^(\d+(?:\.\d+)?)\s*([kmgt]?)(?:i?b)?$/);
  if (!m) return null;
  const bytes = Math.floor(parseFloat(m[1]) * (MULT[m[2]] ?? 1));
  return bytes > 0 ? bytes : null;
}

const styles: Record<string, CSSProperties> = {
  row: { display: "flex", gap: 8, alignItems: "center" },
  list: {
    margin: "8px 0 0",
    padding: 0,
    listStyle: "none",
    maxHeight: 120,
    overflowY: "auto",
    border: "1px solid #eeeeee",
    borderRadius: 4,
  },
  listItem: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
    padding: "2px 4px 2px 8px",
    fontSize: 12,
  },
  path: { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  grid: { display: "flex", gap: 12, marginTop: 12 },
  advanced: {
    marginTop: 12,
    border: "1px solid #e0e0e0",
    borderRadius: 6,
    padding: "8px 12px 12px",
    display: "flex",
    flexDirection: "column",
    gap: 10,
  },
  legend: { fontSize: 12, color: "#666666", padding: "0 4px" },
};

/** New-archive wizard: sources → destination → format/compression options. */
export default function CreateWizard({ open, onClose, onStart }: Props) {
  const [sources, setSources] = useState<string[]>([]);
  const [dest, setDest] = useState("");
  const [format, setFormat] = useState<string>("zip");
  const [level, setLevel] = useState<string>("normal");
  const [method, setMethod] = useState<string>("auto");
  const [password, setPassword] = useState("");
  const [encryptNames, setEncryptNames] = useState(false);
  const [volume, setVolume] = useState<string>("off");
  const [customVolume, setCustomVolume] = useState("");
  const [sfx, setSfx] = useState(false);
  const [sfxKind, setSfxKind] = useState<"gui" | "console">("gui");
  const [touched, setTouched] = useState(false);

  useEffect(() => {
    if (open) {
      setSources([]);
      setDest("");
      setFormat("zip");
      setLevel("normal");
      setMethod("auto");
      setPassword("");
      setEncryptNames(false);
      setVolume("off");
      setCustomVolume("");
      setSfx(false);
      setSfxKind("gui");
      setTouched(false);
    }
  }, [open]);

  // SFX is a 7z-only executable stub, so it pins the format and forbids
  // splitting. TAR has no encryption; the engine returns
  // `error.password_unsupported`, so the password field is disabled instead.
  const effFormat = sfx ? "7z" : format;
  const isTar = effFormat.startsWith("tar");
  const is7z = effFormat === "7z";
  const hasPassword = password.trim() !== "";
  // 7z header encryption needs a key; without one the engine errors, so the
  // checkbox only enables once a password is present.
  const encryptNamesEnabled = is7z && hasPassword;
  // Only single-container ZIP/7z can be split (TAR family and SFX cannot).
  const volumeEnabled = !sfx && (effFormat === "zip" || effFormat === "7z");

  const addSources = (paths: string[]) =>
    setSources((prev) => [...prev, ...paths.filter((p) => !prev.includes(p))]);

  // The dialog can return a single path (string) or several (string[]) depending
  // on platform/mode; accept both so a single pick is never silently dropped.
  const addPicked = (picked: string | string[] | null) => {
    if (Array.isArray(picked)) addSources(picked);
    else if (typeof picked === "string" && picked) addSources([picked]);
  };

  const pickFiles = async () => {
    try {
      addPicked(await openDialog({ multiple: true }));
    } catch (e) {
      console.error("pick files failed", e);
    }
  };

  const pickFolders = async () => {
    try {
      addPicked(await openDialog({ directory: true, multiple: true }));
    } catch (e) {
      console.error("pick folders failed", e);
    }
  };

  const browseDest = async () => {
    const picked = await saveDialog({});
    if (typeof picked === "string") setDest(picked);
  };

  const onSfxChange = (checked: boolean) => {
    setSfx(checked);
    if (checked) {
      setFormat("7z");
      setVolume("off");
      setCustomVolume("");
    }
  };

  const customBytes = volume === "custom" ? parseVolume(customVolume) : null;
  const sourcesError = touched && sources.length === 0;
  const destError = touched && !dest.trim();
  const volumeError = volumeEnabled && volume === "custom" && customBytes === null;

  const start = () => {
    setTouched(true);
    const volumeBytes =
      volume === "off" ? null : volume === "custom" ? customBytes : parseVolume(volume);
    const invalidVolume = volumeEnabled && volume !== "off" && volumeBytes === null;
    if (sources.length === 0 || !dest.trim() || invalidVolume) return;
    onStart(sources, dest.trim(), {
      format: effFormat,
      level,
      method,
      password: !isTar && hasPassword ? password : null,
      encrypt_names: encryptNamesEnabled && encryptNames,
      volume_bytes: volumeEnabled ? volumeBytes : null,
      sfx: sfx ? sfxKind : null,
    });
  };

  return (
    <Dialog modalType="modal" open={open} onOpenChange={(_, d) => !d && onClose()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("create.title")}</DialogTitle>
          <DialogContent>
            <Field
              label={t("create.sources")}
              validationMessage={sourcesError ? t("create.no_sources") : ""}
              validationState={sourcesError ? "error" : "none"}
            >
              <div style={styles.row}>
                <Button onClick={() => void pickFiles()}>{t("create.add_files")}</Button>
                <Button onClick={() => void pickFolders()}>{t("create.add_folder")}</Button>
              </div>
            </Field>
            {sources.length > 0 && (
              <ul style={styles.list}>
                {sources.map((p) => (
                  <li key={p} style={styles.listItem}>
                    <span style={styles.path} title={p}>
                      {p}
                    </span>
                    <Button
                      appearance="subtle"
                      size="small"
                      aria-label={t("create.remove")}
                      onClick={() => setSources((prev) => prev.filter((x) => x !== p))}
                    >
                      ✕
                    </Button>
                  </li>
                ))}
              </ul>
            )}

            <Field
              label={t("create.dest")}
              validationMessage={destError ? t("create.pick_dest") : ""}
              validationState={destError ? "error" : "none"}
              style={{ marginTop: 12 }}
            >
              <div style={styles.row}>
                <Input value={dest} onChange={(_, d) => setDest(d.value)} style={{ flex: 1 }} />
                <Button onClick={() => void browseDest()}>{t("create.browse")}</Button>
              </div>
            </Field>

            <div style={styles.grid}>
              <Field label={t("create.format")} style={{ flex: 1 }}>
                <Dropdown
                  value={t(FORMAT_KEY[effFormat] ?? "create.format.zip")}
                  selectedOptions={[effFormat]}
                  onOptionSelect={(_, d) => setFormat(d.optionValue ?? "zip")}
                  disabled={sfx}
                >
                  {FORMATS.map((f) => (
                    <Option key={f.value} value={f.value}>
                      {t(f.key)}
                    </Option>
                  ))}
                </Dropdown>
              </Field>
              <Field label={t("create.level")} style={{ flex: 1 }}>
                <Dropdown
                  value={t(`create.level.${level}`)}
                  selectedOptions={[level]}
                  onOptionSelect={(_, d) => setLevel(d.optionValue ?? "normal")}
                >
                  {LEVELS.map((l) => (
                    <Option key={l} value={l}>
                      {t(`create.level.${l}`)}
                    </Option>
                  ))}
                </Dropdown>
              </Field>
            </div>

            <fieldset style={styles.advanced}>
              <legend style={styles.legend}>{t("create.advanced")}</legend>

              <Field label={t("create.method")}>
                <Dropdown
                  value={t(`create.method.${method}`)}
                  selectedOptions={[method]}
                  onOptionSelect={(_, d) => setMethod(d.optionValue ?? "auto")}
                >
                  {METHODS.map((m) => (
                    <Option key={m} value={m}>
                      {t(`create.method.${m}`)}
                    </Option>
                  ))}
                </Dropdown>
              </Field>

              <Field label={t("create.password")}>
                <Input
                  type="password"
                  value={password}
                  disabled={isTar}
                  onChange={(_, d) => setPassword(d.value)}
                />
              </Field>

              <Checkbox
                checked={encryptNamesEnabled && encryptNames}
                disabled={!encryptNamesEnabled}
                onChange={(_, c) => setEncryptNames(Boolean(c.checked))}
                label={t("create.encrypt_names")}
              />

              <Field
                label={t("create.volume")}
                validationMessage={volumeError ? t("create.volume.invalid") : ""}
                validationState={volumeError ? "error" : "none"}
              >
                <div style={styles.row}>
                  <Dropdown
                    value={t(`create.volume.${volume}`)}
                    selectedOptions={[volume]}
                    onOptionSelect={(_, d) => setVolume(d.optionValue ?? "off")}
                    disabled={!volumeEnabled}
                  >
                    {VOLUMES.map((v) => (
                      <Option key={v} value={v}>
                        {t(`create.volume.${v}`)}
                      </Option>
                    ))}
                  </Dropdown>
                  {volumeEnabled && volume === "custom" && (
                    <Input
                      value={customVolume}
                      onChange={(_, d) => setCustomVolume(d.value)}
                      placeholder={t("create.volume.custom_placeholder")}
                    />
                  )}
                </div>
              </Field>

              <Checkbox
                checked={sfx}
                onChange={(_, c) => onSfxChange(Boolean(c.checked))}
                label={t("create.sfx")}
              />

              {sfx && (
                <RadioGroup
                  value={sfxKind}
                  onChange={(_, d) => setSfxKind(d.value as "gui" | "console")}
                >
                  <Radio value="gui" label={t("create.sfx.gui")} />
                  <Radio value="console" label={t("create.sfx.console")} />
                </RadioGroup>
              )}
            </fieldset>
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose}>
              {t("create.cancel")}
            </Button>
            <Button appearance="primary" onClick={start}>
              {t("create.start")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

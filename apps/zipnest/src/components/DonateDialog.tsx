import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
} from "@fluentui/react-components";
import { t } from "../i18n";
import alipayImg from "../assets/donate/alipay.jpg";
import wechatImg from "../assets/donate/wechat.jpg";

interface Props {
  open: boolean;
  onClose: () => void;
}

/** A quiet "support us" dialog: short copy + two payment QR codes. The QR
 * images live in `src/assets/donate/` — replace them with real ones. */
export default function DonateDialog({ open, onClose }: Props) {
  return (
    <Dialog modalType="modal" open={open} onOpenChange={(_, d) => !d && onClose()}>
      <DialogSurface style={{ maxWidth: 560 }}>
        <DialogBody>
          <DialogTitle>{t("settings.donate.title")}</DialogTitle>
          <DialogContent>
            <p style={{ marginTop: 0, color: "#555" }}>{t("settings.donate.body")}</p>
            <div
              style={{
                display: "flex",
                gap: 24,
                justifyContent: "center",
                alignItems: "flex-start",
                marginTop: 12,
              }}
            >
              <div style={{ textAlign: "center" }}>
                {/* 240 px wide so phones can scan it comfortably; the QR files
                    themselves stay full-res in src/assets/donate/. */}
                <img
                  src={wechatImg}
                  alt={t("settings.donate.wechat")}
                  style={{ width: 240, height: "auto", display: "block" }}
                />
                <div style={{ fontSize: 14, color: "#666", marginTop: 4 }}>
                  {t("settings.donate.wechat")}
                </div>
              </div>
              <div style={{ textAlign: "center" }}>
                <img
                  src={alipayImg}
                  alt={t("settings.donate.alipay")}
                  style={{ width: 240, height: "auto", display: "block" }}
                />
                <div style={{ fontSize: 14, color: "#666", marginTop: 4 }}>
                  {t("settings.donate.alipay")}
                </div>
              </div>
            </div>
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose}>
              {t("extract.cancel")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

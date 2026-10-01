import { Dialog, DialogBody, DialogContent, DialogSurface, DialogTitle } from "@fluentui/react-components";
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
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("settings.donate.title")}</DialogTitle>
          <DialogContent>
            <p style={{ marginTop: 0, color: "#555" }}>{t("settings.donate.body")}</p>
            <div style={{ display: "flex", gap: 16, justifyContent: "center", marginTop: 8 }}>
              <div style={{ textAlign: "center" }}>
                <img src={wechatImg} alt={t("settings.donate.wechat")} width={140} height={176} />
                <div style={{ fontSize: 12, color: "#666" }}>{t("settings.donate.wechat")}</div>
              </div>
              <div style={{ textAlign: "center" }}>
                <img src={alipayImg} alt={t("settings.donate.alipay")} width={140} height={176} />
                <div style={{ fontSize: 12, color: "#666" }}>{t("settings.donate.alipay")}</div>
              </div>
            </div>
          </DialogContent>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Monitor, Square } from "lucide-react";
import { useAppStore } from "../../../store";
import { stopNativeBroadcast, useNativeBroadcast } from "./nativeBroadcast";
import styles from "./ScreenShareViewer.module.css";

export function SharingToolbar({ onStop, previewVisible, onPreviewChange }: {
  readonly onStop: () => void;
  readonly previewVisible: boolean;
  readonly onPreviewChange: (visible: boolean) => void;
}) {
  const { t } = useTranslation(["chat", "settings"]);
  const sharing = useAppStore((s) => s.isSharingOwn);
  const { broadcast, stopping } = useNativeBroadcast();
  const [error, setError] = useState("");
  if (!sharing && !broadcast?.status?.running) return null;
  async function stop() {
    setError("");
    try { if (broadcast) await stopNativeBroadcast(); else onStop(); }
    catch (e) { setError(String(e)); }
  }
  return <div className={styles.sharingToolbar}>
    <Monitor size={18} aria-hidden="true" />
    <span className={styles.sharingSource} title={broadcast?.sourceName}>
      {t("settings:screenShare.running")}{broadcast?.sourceName ? `: ${broadcast.sourceName}` : ""}
    </span>
    <label className={styles.previewToggle}><input type="checkbox" checked={previewVisible}
      onChange={(e) => onPreviewChange(e.target.checked)} />{t("screenShare.preview")}</label>
    <button className={styles.stopSharingButton} disabled={stopping} onClick={() => { void stop(); }}>
      <Square size={14} aria-hidden="true" />{t("screenShare.stopSharing")}
    </button>
    {error && <span role="alert" className={styles.sharingError}>{error}</span>}
  </div>;
}

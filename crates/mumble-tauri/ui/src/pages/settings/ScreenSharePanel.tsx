import { useEffect, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { RefreshCw, Save } from "lucide-react";
import { ScreenShareOptions } from "../../components/chat/stream/ScreenShareOptions";
import { loadSharePreferences, saveSharePreferences, type EncoderReport, type SharePreferences } from "../../components/chat/stream/nativeSettings";
import { registerSettings } from "./settingsSearchRegistry";
import styles from "../../components/chat/stream/ScreenShareSetup.module.css";

registerSettings("screen-share")
  .add("screenShare.encoder")
  .add("screenShare.resolution")
  .add("screenShare.fps")
  .add("screenShare.bitrate")
  .add("screenShare.audio")
  .add("screenShare.viewerStats")
  .add("screenShare.showViewerStats")
  .add("screenShare.statsConnection")
  .add("screenShare.statsVideo")
  .add("screenShare.statsNetwork");

export function ScreenSharePanel() {
  const { t } = useTranslation("settings");
  const [value, setValue] = useState<SharePreferences | null>(null);
  const [report, setReport] = useState<EncoderReport | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  useEffect(() => {
    let cancelled = false;
    Promise.all([loadSharePreferences(), invoke<EncoderReport>("list_video_encoders")])
      .then(([preferences, encoders]) => { if (!cancelled) { setValue(preferences); setReport(encoders); } })
      .catch((e: unknown) => { if (!cancelled) setError(String(e)); })
      .finally(() => { if (!cancelled) setBusy(false); });
    return () => { cancelled = true; };
  }, []);
  async function save(event: FormEvent) {
    event.preventDefault();
    if (!value) return;
    setBusy(true); setError("");
    try { await saveSharePreferences(value); setSaved(true); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function refresh() {
    setBusy(true); setError("");
    try { setReport(await invoke<EncoderReport>("list_video_encoders", { refresh: true })); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  return <form className={styles.panel} onSubmit={(e) => { void save(e); }}>
    <h2 className={styles.heading}>{t("screenShare.defaults")}</h2>
    {busy && <p role="status" className={styles.status}>{t("screenShare.loading")}</p>}
    {error && <p role="alert" className={styles.error}>{error}</p>}
    {report && !report.supported && <p className={styles.status}>{t("screenShare.unsupported")}</p>}
    {value && report?.supported &&
      <fieldset disabled={busy} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
        <ScreenShareOptions value={value} report={report} onChange={(next) => { setValue(next); setSaved(false); }} />
      </fieldset>
    }
    {value && <>
      <h2 className={`${styles.heading} ${styles.sectionHeading}`}>{t("screenShare.viewerStats")}</h2>
      <p className={styles.status}>{t("screenShare.viewerStatsHint")}</p>
      <fieldset disabled={busy} style={{ border: 0, padding: 0, margin: "14px 0 0", minWidth: 0 }}>
        <div className={styles.options}>
          <label className={`${styles.check} ${styles.wide}`}>
            <input type="checkbox" checked={value.viewerStats.enabled} onChange={(e) => {
              setValue({ ...value, viewerStats: { ...value.viewerStats, enabled: e.target.checked } });
              setSaved(false);
            }} />
            {t("screenShare.showViewerStats")}
          </label>
          <label className={styles.check}>
            <input type="checkbox" disabled={!value.viewerStats.enabled} checked={value.viewerStats.connection} onChange={(e) => {
              setValue({ ...value, viewerStats: { ...value.viewerStats, connection: e.target.checked } });
              setSaved(false);
            }} />
            {t("screenShare.statsConnection")}
          </label>
          <label className={styles.check}>
            <input type="checkbox" disabled={!value.viewerStats.enabled} checked={value.viewerStats.video} onChange={(e) => {
              setValue({ ...value, viewerStats: { ...value.viewerStats, video: e.target.checked } });
              setSaved(false);
            }} />
            {t("screenShare.statsVideo")}
          </label>
          <label className={styles.check}>
            <input type="checkbox" disabled={!value.viewerStats.enabled} checked={value.viewerStats.network} onChange={(e) => {
              setValue({ ...value, viewerStats: { ...value.viewerStats, network: e.target.checked } });
              setSaved(false);
            }} />
            {t("screenShare.statsNetwork")}
          </label>
        </div>
      </fieldset>
      <div className={styles.actions}>
        <button className={`${styles.button} ${styles.primary}`} type="submit" disabled={busy}><Save size={16} />{t(saved ? "screenShare.saved" : "screenShare.saveDefaults")}</button>
        {report?.supported && <button className={styles.iconButton} type="button" disabled={busy} title={t("screenShare.refreshEncoders")} aria-label={t("screenShare.refreshEncoders")} onClick={() => { void refresh(); }}><RefreshCw size={16} /></button>}
      </div>
    </>}
  </form>;
}

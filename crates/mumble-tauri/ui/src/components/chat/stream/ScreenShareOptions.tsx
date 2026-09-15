import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Accordion } from "../../../pages/settings/SharedControls";
import type { EncoderReport, SharePreferences, ScreenShareSettings } from "./nativeSettings";
import styles from "./ScreenShareSetup.module.css";

interface Props {
  value: SharePreferences;
  report: EncoderReport;
  onChange: (value: SharePreferences) => void;
  suggestedBitrate?: number;
}

export function ScreenShareOptions({ value, report, onChange, suggestedBitrate }: Props) {
  const { t } = useTranslation("settings");
  const id = useId();
  const settings = value.settings;
  const update = (patch: Partial<ScreenShareSettings>) => onChange({ ...value, settings: { ...settings, ...patch } });
  const resolution = settings.resolution;
  const resolutionValue = resolution.mode === "preset" ? String(resolution.lines) : resolution.mode;
  const selected = report.encoders.find((e) => e.id === settings.encoder);
  return <>
    <div className={styles.options}>
      <label className={styles.wide} htmlFor={`${id}-encoder`}>{t("screenShare.encoder")}
        <select id={`${id}-encoder`} value={settings.encoder} onChange={(e) => update({ encoder: e.target.value })}>
          <option value="auto" disabled={!report.autoSelected}>{t("screenShare.auto")}{report.autoSelected ? ` (${report.autoSelected})` : ""}</option>
          {settings.encoder !== "auto" && !selected && <option disabled value={settings.encoder}>{settings.encoder}</option>}
          {report.encoders.map((encoder) => <option key={encoder.id} value={encoder.id} disabled={!encoder.available} title={encoder.detail ?? undefined}>
            {encoder.displayName}{encoder.available ? "" : ` - ${encoder.detail || t("screenShare.unavailable")}`}
          </option>)}
        </select>
      </label>
      {selected && !selected.available && <p role="alert" className={styles.error}>{selected.detail || t("screenShare.unavailable")}</p>}
      <label htmlFor={`${id}-resolution`}>{t("screenShare.resolution")}
        <select id={`${id}-resolution`} value={resolutionValue} onChange={(e) => {
          const next = e.target.value;
          update({ resolution: next === "native" ? { mode: "native" } : next === "custom" ? { mode: "custom", width: 1920, height: 1080 } : { mode: "preset", lines: Number(next) } });
        }}>
          <option value="native">{t("screenShare.native")}</option>
          {[2160, 1440, 1080, 720].map((lines) => <option key={lines} value={lines}>{lines}p</option>)}
          <option value="custom">{t("screenShare.custom")}</option>
        </select>
      </label>
      <label htmlFor={`${id}-fps`}>{t("screenShare.fps")}
        <select id={`${id}-fps`} value={settings.fps} onChange={(e) => update({ fps: Number(e.target.value) })}>
          {[...new Set([15, 30, 60, settings.fps])].sort((a, b) => a - b).map((fps) => <option key={fps} value={fps}>{fps} FPS</option>)}
        </select>
      </label>
      {resolution.mode === "custom" && <>
        <label htmlFor={`${id}-width`}>{t("screenShare.width")}<input id={`${id}-width`} type="number" required min={16} max={8192} step={1} value={resolution.width || ""} onChange={(e) => update({ resolution: { ...resolution, width: Number(e.target.value) } })} /></label>
        <label htmlFor={`${id}-height`}>{t("screenShare.height")}<input id={`${id}-height`} type="number" required min={16} max={8192} step={1} value={resolution.height || ""} onChange={(e) => update({ resolution: { ...resolution, height: Number(e.target.value) } })} /></label>
      </>}
      <label htmlFor={`${id}-bitrate-mode`}>{t("screenShare.bitrate")}
        <select id={`${id}-bitrate-mode`} value={settings.bitrateKbps === null ? "auto" : "manual"} onChange={(e) => update({ bitrateKbps: e.target.value === "auto" ? null : suggestedBitrate ?? 5000 })}>
          <option value="auto">{t("screenShare.auto")}</option><option value="manual">{t("screenShare.manual")}</option>
        </select>
      </label>
      {settings.bitrateKbps !== null && <label htmlFor={`${id}-bitrate`}>kbps<input id={`${id}-bitrate`} type="number" required min={500} max={50000} step={1} value={settings.bitrateKbps || ""} onChange={(e) => update({ bitrateKbps: Number(e.target.value) })} /></label>}
      {suggestedBitrate !== undefined && <output className={styles.wide}>{t("screenShare.suggested", { bitrate: suggestedBitrate })}</output>}
    </div>
    <Accordion title={t("screenShare.advanced")}>
      <div className={styles.options}>
        <label className={styles.wide} htmlFor={`${id}-capture`}>{t("screenShare.capture")}
          <select id={`${id}-capture`} value={settings.capture} onChange={(e) => update({ capture: e.target.value as ScreenShareSettings["capture"] })}>
            <option value="ddagrab">DXGI Desktop Duplication (ddagrab)</option>
            <option value="gfxcapture">Windows Graphics Capture (gfxcapture)</option>
          </select>
        </label>
        <label className={styles.check}><input type="checkbox" checked={value.drawCursor} onChange={(e) => onChange({ ...value, drawCursor: e.target.checked })} />{t("screenShare.cursor")}</label>
        <label className={styles.check}><input type="checkbox" checked={settings.p2p === "auto"} onChange={(e) => update({ p2p: e.target.checked ? "auto" : "disabled" })} />{t("screenShare.p2p")}</label>
        {settings.p2p === "auto" && <label htmlFor={`${id}-viewers`}>{t("screenShare.maxViewers")}<input id={`${id}-viewers`} type="number" required min={0} max={8} step={1} value={settings.p2pMaxViewers} onChange={(e) => update({ p2pMaxViewers: Number(e.target.value) })} /></label>}
      </div>
    </Accordion>
  </>;
}

import { Activity, Cable, Cpu, Download, Film, Gauge, LoaderCircle, Monitor, Server, TriangleAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { DEFAULT_VIEWER_STATS_PREFERENCES, type ViewerStatsPreferences } from "./nativeSettings";
import type { ViewerConnectionStats } from "./viewerStats";
import styles from "./ScreenShareViewer.module.css";

interface Props {
  readonly stats: ViewerConnectionStats;
  readonly preferences?: Pick<ViewerStatsPreferences, "connection" | "video" | "network">;
}

export function StreamConnectionStats({ stats, preferences = DEFAULT_VIEWER_STATS_PREFERENCES }: Props) {
  const { t } = useTranslation("chat");
  const Icon = stats.route === "p2p" ? Cable : stats.route === "sfu" ? Server : LoaderCircle;
  const route = stats.route === "connecting" ? t("screenShare.connecting") : stats.route.toUpperCase();
  const rate = stats.bitrateKbps === null ? "--" : stats.bitrateKbps >= 1000
    ? `${(stats.bitrateKbps / 1000).toFixed(2)} Mbps` : `${Math.round(stats.bitrateKbps)} kbps`;
  const resolution = stats.width === null || stats.height === null ? "--" : `${stats.width} x ${stats.height}`;
  const frameRate = stats.fps === null ? "--" : `${Math.round(stats.fps)} FPS`;
  const jitter = stats.jitterMs === null ? "--" : `${stats.jitterMs.toFixed(1)} ms`;
  const loss = stats.packetLossPercent === null ? "--"
    : `${stats.packetLossPercent.toFixed(1)}% (${Math.round(stats.packetsLost ?? 0)})`;
  const frames = stats.framesDecoded === null ? "--" : Math.round(stats.framesDecoded).toLocaleString();
  const dropped = stats.framesDropped === null ? "--" : Math.round(stats.framesDropped).toLocaleString();
  return <div className={styles.viewerStats} aria-label={t("screenShare.connectionStats")}>
    {preferences.connection && <div className={styles.viewerStatsGroup}>
      <span title={t(`screenShare.route.${stats.route}`)}>
        <Icon size={14} aria-hidden="true" className={stats.route === "connecting" ? styles.statsSpinner : undefined} />{route}
      </span>
      <span title={t("screenShare.rtt")} aria-label={`${t("screenShare.rtt")}: ${stats.rttMs === null ? "--" : `${Math.round(stats.rttMs)} ms`}`}>
        <Gauge size={14} aria-hidden="true" />RTT {stats.rttMs === null ? "--" : `${Math.round(stats.rttMs)} ms`}
      </span>
    </div>}
    {preferences.video && <div className={styles.viewerStatsGroup}>
      <span title={t("screenShare.codec")}><Film size={14} aria-hidden="true" />{t("screenShare.codecShort")} {stats.codec ?? "--"}</span>
      <span title={t("screenShare.decoder")}><Cpu size={14} aria-hidden="true" />{stats.decoder ?? "--"}</span>
      <span title={t("screenShare.resolution")}><Monitor size={14} aria-hidden="true" />{resolution} / {frameRate}</span>
      <span title={t("screenShare.frames")}><Activity size={14} aria-hidden="true" />{frames} / {t("screenShare.droppedFramesShort")} {dropped}</span>
    </div>}
    {preferences.network && <div className={styles.viewerStatsGroup}>
      <span title={t("screenShare.receiveBitrate")} aria-label={`${t("screenShare.receiveBitrate")}: ${rate}`}>
        <Download size={14} aria-hidden="true" />{rate}
      </span>
      <span title={t("screenShare.jitter")}><Activity size={14} aria-hidden="true" />{t("screenShare.jitterShort")} {jitter}</span>
      <span title={t("screenShare.packetLoss")}><TriangleAlert size={14} aria-hidden="true" />{t("screenShare.packetLossShort")} {loss}</span>
    </div>}
  </div>;
}

import { Cable, Download, Gauge, LoaderCircle, Server } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { ViewerConnectionStats } from "./viewerStats";
import styles from "./ScreenShareViewer.module.css";

export function StreamConnectionStats({ stats }: { readonly stats: ViewerConnectionStats }) {
  const { t } = useTranslation("chat");
  const Icon = stats.route === "p2p" ? Cable : stats.route === "sfu" ? Server : LoaderCircle;
  const route = stats.route === "connecting" ? t("screenShare.connecting") : stats.route.toUpperCase();
  const rate = stats.bitrateKbps === null ? "--" : stats.bitrateKbps >= 1000
    ? `${(stats.bitrateKbps / 1000).toFixed(2)} Mbps` : `${Math.round(stats.bitrateKbps)} kbps`;
  return <div className={styles.viewerStats} aria-label={t("screenShare.connectionStats")}>
    <span title={t(`screenShare.route.${stats.route}`)}>
      <Icon size={14} aria-hidden="true" className={stats.route === "connecting" ? styles.statsSpinner : undefined} />{route}
    </span>
    <span title={t("screenShare.rtt")} aria-label={`${t("screenShare.rtt")}: ${stats.rttMs === null ? "--" : `${Math.round(stats.rttMs)} ms`}`}>
      <Gauge size={14} aria-hidden="true" />RTT {stats.rttMs === null ? "--" : `${Math.round(stats.rttMs)} ms`}
    </span>
    <span title={t("screenShare.receiveBitrate")} aria-label={`${t("screenShare.receiveBitrate")}: ${rate}`}>
      <Download size={14} aria-hidden="true" />{rate}
    </span>
  </div>;
}

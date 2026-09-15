export interface ViewerConnectionStats {
  route: "connecting" | "p2p" | "sfu";
  rttMs: number | null;
  bitrateKbps: number | null;
}

export const CONNECTING_STATS: ViewerConnectionStats = { route: "connecting", rttMs: null, bitrateKbps: null };
export type VideoSamples = Map<string, { bytes: number; timestamp: number }>;

function nonnegative(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/** Use video RTP counters, not total connection traffic (audio, STUN and DTLS). */
export function readViewerStats(reports: RTCStatsReport, previous: VideoSamples) {
  const samples: VideoSamples = new Map();
  const transportIds = new Set<string>();
  let bitrateKbps: number | null = null;
  reports.forEach((report) => {
    if (report.type !== "inbound-rtp" || (report.kind ?? report.mediaType) !== "video" || report.isRemote) return;
    if (report.transportId) transportIds.add(report.transportId);
    if (!nonnegative(report.bytesReceived) || !nonnegative(report.timestamp)) return;
    samples.set(report.id, { bytes: report.bytesReceived, timestamp: report.timestamp });
    const last = previous.get(report.id);
    if (last && report.timestamp > last.timestamp && report.bytesReceived >= last.bytes) {
      bitrateKbps = (bitrateKbps ?? 0) + 8 * (report.bytesReceived - last.bytes) / (report.timestamp - last.timestamp);
    }
  });

  let pair: RTCIceCandidatePairStats | undefined;
  reports.forEach((report) => {
    if (report.type === "transport" && report.selectedCandidatePairId
      && (transportIds.size === 0 || transportIds.has(report.id))) {
      pair ??= reports.get(report.selectedCandidatePairId) as RTCIceCandidatePairStats | undefined;
    }
  });
  // Older WebViews expose selection on the candidate-pair report instead.
  if (!pair) reports.forEach((report) => {
    if (report.type === "candidate-pair" && report.state === "succeeded" && (report.selected || report.nominated)) pair ??= report;
  });
  const rttMs = pair?.state === "succeeded" && nonnegative(pair.currentRoundTripTime)
    ? pair.currentRoundTripTime * 1000 : null;
  return { samples, bitrateKbps, rttMs };
}

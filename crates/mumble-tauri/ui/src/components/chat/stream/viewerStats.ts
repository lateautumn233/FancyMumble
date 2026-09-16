export interface ViewerConnectionStats {
  route: "connecting" | "p2p" | "sfu";
  rttMs: number | null;
  bitrateKbps: number | null;
  codec: string | null;
  decoder: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  jitterMs: number | null;
  packetsLost: number | null;
  packetLossPercent: number | null;
  framesDecoded: number | null;
  framesDropped: number | null;
}

export const CONNECTING_STATS: ViewerConnectionStats = {
  route: "connecting",
  rttMs: null,
  bitrateKbps: null,
  codec: null,
  decoder: null,
  width: null,
  height: null,
  fps: null,
  jitterMs: null,
  packetsLost: null,
  packetLossPercent: null,
  framesDecoded: null,
  framesDropped: null,
};
export type VideoSamples = Map<string, {
  bytes: number;
  timestamp: number;
  packetsReceived: number | null;
  packetsLost: number | null;
}>;

function nonnegative(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/** Use video RTP counters, not total connection traffic (audio, STUN and DTLS). */
export function readViewerStats(reports: RTCStatsReport, previous: VideoSamples) {
  const samples: VideoSamples = new Map();
  const transportIds = new Set<string>();
  let bitrateKbps: number | null = null;
  let receivedDelta = 0;
  let lostDelta = 0;
  let hasPacketDelta = false;
  let primary: Record<string, unknown> | null = null;
  reports.forEach((report) => {
    if (report.type !== "inbound-rtp" || (report.kind ?? report.mediaType) !== "video" || report.isRemote) return;
    if (report.transportId) transportIds.add(report.transportId);
    if (!primary || (nonnegative(report.bytesReceived) && report.bytesReceived > Number(primary.bytesReceived ?? -1))) {
      primary = report;
    }
    if (!nonnegative(report.bytesReceived) || !nonnegative(report.timestamp)) return;
    const packetsReceived = nonnegative(report.packetsReceived) ? report.packetsReceived : null;
    const packetsLost = nonnegative(report.packetsLost) ? report.packetsLost : null;
    samples.set(report.id, { bytes: report.bytesReceived, timestamp: report.timestamp, packetsReceived, packetsLost });
    const last = previous.get(report.id);
    if (last && report.timestamp > last.timestamp && report.bytesReceived >= last.bytes) {
      bitrateKbps = (bitrateKbps ?? 0) + 8 * (report.bytesReceived - last.bytes) / (report.timestamp - last.timestamp);
      if (packetsReceived !== null && packetsLost !== null
        && last.packetsReceived !== null && last.packetsLost !== null
        && packetsReceived >= last.packetsReceived && packetsLost >= last.packetsLost) {
        receivedDelta += packetsReceived - last.packetsReceived;
        lostDelta += packetsLost - last.packetsLost;
        hasPacketDelta = true;
      }
    }
  });

  const primaryReport = primary as Record<string, unknown> | null;
  const numberFromPrimary = (key: string): number | null => {
    const value = primaryReport?.[key];
    return nonnegative(value) ? value : null;
  };
  const codecReport = typeof primaryReport?.codecId === "string"
    ? reports.get(primaryReport.codecId) as Record<string, unknown> | undefined
    : undefined;
  const mimeType = typeof codecReport?.mimeType === "string" ? codecReport.mimeType : null;
  const codec = mimeType?.replace(/^video\//i, "") ?? null;
  const decoder = typeof primaryReport?.decoderImplementation === "string"
    ? primaryReport.decoderImplementation : null;
  const jitterSeconds = numberFromPrimary("jitter");
  const packetTotal = receivedDelta + lostDelta;
  const packetLossPercent = hasPacketDelta && packetTotal > 0 ? lostDelta / packetTotal * 100
    : hasPacketDelta ? 0 : null;

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
  return {
    samples,
    bitrateKbps,
    rttMs,
    codec,
    decoder,
    width: numberFromPrimary("frameWidth"),
    height: numberFromPrimary("frameHeight"),
    fps: numberFromPrimary("framesPerSecond"),
    jitterMs: jitterSeconds === null ? null : jitterSeconds * 1000,
    packetsLost: numberFromPrimary("packetsLost"),
    packetLossPercent,
    framesDecoded: numberFromPrimary("framesDecoded"),
    framesDropped: numberFromPrimary("framesDropped"),
  };
}

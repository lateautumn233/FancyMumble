import { describe, expect, it } from "vitest";
import { readViewerStats } from "../chat/stream/viewerStats";

const report = (...entries: Record<string, unknown>[]) => new Map(entries.map(e => [e.id, e])) as unknown as RTCStatsReport;
const video = (bytesReceived: number, timestamp: number) => ({ id: "video", type: "inbound-rtp", kind: "video", bytesReceived, timestamp, transportId: "transport" });

describe("viewer stats", () => {
  it("uses video byte deltas and the selected candidate pair RTT", () => {
    const first = readViewerStats(report(video(100_000, 1000)), new Map());
    expect(first.bitrateKbps).toBeNull();
    const second = readViewerStats(report(
      video(350_000, 2000),
      { id: "audio", type: "inbound-rtp", kind: "audio", bytesReceived: 999_999, timestamp: 2000 },
      { id: "transport", type: "transport", selectedCandidatePairId: "selected" },
      { id: "wrong", type: "candidate-pair", nominated: true, state: "succeeded", currentRoundTripTime: 0.9 },
      { id: "selected", type: "candidate-pair", state: "succeeded", currentRoundTripTime: 0.025 },
    ), first.samples);
    expect(second.bitrateKbps).toBe(2000);
    expect(second.rttMs).toBe(25);
  });
  it("reports zero for stalled video, not stale bitrate", () => {
    const first = readViewerStats(report(video(10, 1000)), new Map());
    expect(readViewerStats(report(video(10, 2000)), first.samples).bitrateKbps).toBe(0);
  });
  it("does not produce negative rates after counters reset or timestamps regress", () => {
    const first = readViewerStats(report(video(100, 1000)), new Map());
    for (const entry of [video(20, 2000), video(200, 500), video(200, 1000)]) {
      expect(readViewerStats(report(entry), first.samples).bitrateKbps).toBeNull();
    }
  });
  it("handles older WebViews and absent or invalid statistics", () => {
    const result = readViewerStats(report(
      { id: "legacy", type: "inbound-rtp", mediaType: "video", bytesReceived: 50, timestamp: 1000 },
      { id: "pair", type: "candidate-pair", selected: true, state: "succeeded", currentRoundTripTime: 0 },
    ), new Map());
    expect(result.samples.size).toBe(1);
    expect(result.rttMs).toBe(0);
    expect(readViewerStats(report(), result.samples)).toMatchObject({ samples: new Map(), rttMs: null, bitrateKbps: null });
    expect(readViewerStats(report(video(NaN, 2000), { id: "pair", type: "candidate-pair", nominated: true, state: "succeeded", currentRoundTripTime: -1 }), result.samples).rttMs).toBeNull();
  });
  it("sums established video streams without reusing counters from replaced SSRCs", () => {
    const first = readViewerStats(report(video(100, 1000), { ...video(200, 1000), id: "video2" }), new Map());
    const result = readViewerStats(report(video(200, 2000), { ...video(500, 2000), id: "video2" }, { ...video(99999, 2000), id: "new" }), first.samples);
    expect(result.bitrateKbps).toBe(3.2);
  });
  it("reads video diagnostics and calculates interval packet loss", () => {
    const first = readViewerStats(report({
      ...video(100_000, 1000),
      packetsReceived: 100,
      packetsLost: 2,
      codecId: "codec",
    }, { id: "codec", type: "codec", mimeType: "video/H264" }), new Map());
    const second = readViewerStats(report({
      ...video(200_000, 2000),
      packetsReceived: 110,
      packetsLost: 5,
      codecId: "codec",
      decoderImplementation: "D3D11 Video Decoder",
      frameWidth: 1920,
      frameHeight: 1080,
      framesPerSecond: 59.8,
      jitter: 0.004,
      framesDecoded: 1200,
      framesDropped: 4,
    }, { id: "codec", type: "codec", mimeType: "video/H264" }), first.samples);
    expect(second).toMatchObject({
      codec: "H264",
      decoder: "D3D11 Video Decoder",
      width: 1920,
      height: 1080,
      fps: 59.8,
      jitterMs: 4,
      packetsLost: 5,
      framesDecoded: 1200,
      framesDropped: 4,
    });
    expect(second.packetLossPercent).toBeCloseTo(3 / 13 * 100);
  });
});

import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { StreamConnectionStats } from "../chat/stream/StreamConnectionStats";
import { CONNECTING_STATS } from "../chat/stream/viewerStats";

describe("stream connection statistics", () => {
  it("shows connecting without inventing measurements", () => {
    render(<StreamConnectionStats stats={CONNECTING_STATS} />);
    expect(screen.getByText("Connecting...")).toBeTruthy();
    expect(screen.getByLabelText("Network round-trip time (RTT): --")).toBeTruthy();
    expect(screen.getByLabelText("Video receive bitrate: --")).toBeTruthy();
  });
  it("shows actual route and appropriate bitrate units", () => {
    const { rerender } = render(<StreamConnectionStats stats={{ ...CONNECTING_STATS, route: "p2p", rttMs: 23.8, bitrateKbps: 1234 }} />);
    expect(screen.getByText("P2P")).toBeTruthy();
    expect(screen.getByText("RTT 24 ms")).toBeTruthy();
    expect(screen.getByText("1.23 Mbps")).toBeTruthy();
    rerender(<StreamConnectionStats stats={{ ...CONNECTING_STATS, route: "sfu", bitrateKbps: 450 }} />);
    expect(screen.getByText("SFU")).toBeTruthy();
    expect(screen.getByText("450 kbps")).toBeTruthy();
  });
  it("shows video and packet diagnostics and respects section preferences", () => {
    const stats = {
      ...CONNECTING_STATS,
      route: "p2p" as const,
      codec: "H264",
      decoder: "D3D11 Video Decoder",
      width: 1920,
      height: 1080,
      fps: 60,
      jitterMs: 4.2,
      packetsLost: 7,
      packetLossPercent: 1.5,
      framesDecoded: 1234,
      framesDropped: 3,
    };
    const { rerender } = render(<StreamConnectionStats stats={stats} />);
    expect(screen.getByText("Codec H264")).toBeTruthy();
    expect(screen.getByText("D3D11 Video Decoder")).toBeTruthy();
    expect(screen.getByText("1920 x 1080 / 60 FPS")).toBeTruthy();
    expect(screen.getByText("Loss 1.5% (7)")).toBeTruthy();

    rerender(<StreamConnectionStats stats={stats} preferences={{ connection: true, video: false, network: false }} />);
    expect(screen.queryByText("Codec H264")).toBeNull();
    expect(screen.queryByText("Loss 1.5% (7)")).toBeNull();
    expect(screen.getByText("P2P")).toBeTruthy();
  });
});

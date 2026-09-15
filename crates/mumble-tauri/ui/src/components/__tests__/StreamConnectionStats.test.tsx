import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { StreamConnectionStats } from "../chat/stream/StreamConnectionStats";

describe("stream connection statistics", () => {
  it("shows connecting without inventing measurements", () => {
    render(<StreamConnectionStats stats={{ route: "connecting", rttMs: null, bitrateKbps: null }} />);
    expect(screen.getByText("Connecting...")).toBeTruthy();
    expect(screen.getByLabelText("Network round-trip time (RTT): --")).toBeTruthy();
    expect(screen.getByLabelText("Video receive bitrate: --")).toBeTruthy();
  });
  it("shows actual route and appropriate bitrate units", () => {
    const { rerender } = render(<StreamConnectionStats stats={{ route: "p2p", rttMs: 23.8, bitrateKbps: 1234 }} />);
    expect(screen.getByText("P2P")).toBeTruthy();
    expect(screen.getByText("RTT 24 ms")).toBeTruthy();
    expect(screen.getByText("1.23 Mbps")).toBeTruthy();
    rerender(<StreamConnectionStats stats={{ route: "sfu", rttMs: null, bitrateKbps: 450 }} />);
    expect(screen.getByText("SFU")).toBeTruthy();
    expect(screen.getByText("450 kbps")).toBeTruthy();
  });
});

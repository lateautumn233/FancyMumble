import { act, renderHook, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn().mockResolvedValue(undefined) }));
vi.mock("../chat/drawing/DrawingOverlay", () => ({
  clearAllStrokesInChannel: vi.fn(), clearStrokesFromSender: vi.fn(),
}));
vi.mock("../chat/stream/useStreamPreview", () => ({
  getPreviewPc: () => null, handlePreviewAnswer: vi.fn(), handlePreviewIceCandidate: vi.fn(),
  clearThumbnail: vi.fn(), closePreview: vi.fn(), storeLocalThumbnail: vi.fn().mockResolvedValue(undefined),
}));

import { useAppStore } from "../../store";
import { useRemoteConnectionStats, useScreenShare } from "../chat/stream/useScreenShare";
import { dispatchWebRtcSignal, forgetBroadcasts } from "../chat/stream/screenShareSignals";

class Peer {
  static instances: Peer[] = [];
  connectionState = "new";
  signalingState = "stable";
  remoteDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: (() => void) | null = null;
  onicecandidate: ((event: { candidate: { toJSON: () => RTCIceCandidateInit } }) => void) | null = null;
  close = vi.fn();
  getStats = vi.fn().mockResolvedValue(new Map());
  addTransceiver = vi.fn();
  addIceCandidate = vi.fn().mockResolvedValue(undefined);
  createOffer = vi.fn().mockResolvedValue({ type: "offer", sdp: "sfu-offer" });
  createAnswer = vi.fn().mockResolvedValue({ type: "answer", sdp: "direct-answer" });
  setLocalDescription = vi.fn().mockResolvedValue(undefined);
  setRemoteDescription = vi.fn(async (description: RTCSessionDescriptionInit) => { this.remoteDescription = description; });
  constructor() { Peer.instances.push(this); }
}

const send = vi.fn();
async function signal(kind: number, payload = "", server = "server-a") {
  await act(async () => { dispatchWebRtcSignal(42, 1, kind, payload, server); });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("RTCPeerConnection", Peer);
  Peer.instances = [];
  send.mockClear();
  useAppStore.setState({
    activeServerId: "server-a", ownSession: 1, users: [], currentChannel: 0,
    broadcastingSessions: new Set(), broadcastingOwnSession: null,
    watchingOwnSession: null, watchingSession: null, isSharingOwn: false,
    serverConfig: { ...useAppStore.getState().serverConfig, webrtc_sfu_available: true, webrtc_p2p_relay_available: true },
    sendWebRtcSignal: send,
  });
});

afterEach(async () => {
  await act(async () => {
    useAppStore.setState({ activeServerId: "server-a" });
    forgetBroadcasts("server-a");
    forgetBroadcasts("server-b");
  });
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("native broadcast P2P negotiation", () => {
  it("restores a broadcast announced before the chat view mounted, including its P2P capabilities", async () => {
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    expect(send).not.toHaveBeenCalled();
    const { result } = renderHook(() => useScreenShare());
    expect(result.current.broadcastingSessions.has(42)).toBe(true);
    expect(send).toHaveBeenCalledWith(42, 8, "", "server-a");
    expect(Peer.instances).toHaveLength(1);
    expect(Peer.instances[0].createOffer).not.toHaveBeenCalled();
  });

  it("replays an early announcement once the active server and own session become known", async () => {
    useAppStore.setState({ activeServerId: null, ownSession: null });
    const { result } = renderHook(() => useScreenShare());
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    expect(result.current.broadcastingSessions.size).toBe(0);
    await act(async () => { useAppStore.setState({ activeServerId: "server-a", ownSession: 1 }); });
    expect(result.current.broadcastingSessions.has(42)).toBe(true);
    expect(send).toHaveBeenCalledWith(42, 8, "", "server-a");
  });

  it("does not restore a broadcast stopped before the chat view mounted", async () => {
    await signal(0, "");
    await signal(1);
    const { result } = renderHook(() => useScreenShare());
    expect(result.current.broadcastingSessions.size).toBe(0);
    expect(send).not.toHaveBeenCalled();
  });

  it("restores legacy SFU announcements without requiring a native payload", async () => {
    await signal(0, "");
    const { result } = renderHook(() => useScreenShare());
    await act(async () => {});
    expect(result.current.broadcastingSessions.has(42)).toBe(true);
    expect(send).toHaveBeenCalledWith(42, 2, "sfu-offer", "server-a");
  });

  it("does not show another server's cached broadcasts", async () => {
    await signal(0, "", "server-b");
    const { result } = renderHook(() => useScreenShare());
    expect(result.current.broadcastingSessions.size).toBe(0);
    expect(send).not.toHaveBeenCalled();
    await act(async () => { useAppStore.setState({ activeServerId: "server-b" }); });
    expect(result.current.broadcastingSessions.has(42)).toBe(true);
    expect(send).toHaveBeenCalledWith(42, 2, "sfu-offer", "server-b");
  });

  it("reports actual connected route, resets on SFU fallback and clears after STOP", async () => {
    const { result } = renderHook(() => { useScreenShare(); return useRemoteConnectionStats(42); });
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    expect(result.current.route).toBe("connecting");
    await signal(5, "direct-offer");
    expect(result.current.route).toBe("connecting");
    const direct = Peer.instances[0];
    direct.connectionState = "connected";
    direct.onconnectionstatechange?.();
    direct.getStats.mockResolvedValue(new Map([["video", { id: "video", type: "inbound-rtp", kind: "video", timestamp: 1000, bytesReceived: 100 }]]));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current.route).toBe("p2p");
    direct.getStats.mockResolvedValue(new Map([["video", { id: "video", type: "inbound-rtp", kind: "video", timestamp: 2000, bytesReceived: 1100 }]]));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current.bitrateKbps).toBe(8);
    await signal(9);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current).toEqual({ route: "connecting", rttMs: null, bitrateKbps: null });
    const sfu = Peer.instances[1];
    sfu.connectionState = "connected";
    sfu.onconnectionstatechange?.();
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current.route).toBe("sfu");
    await signal(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current).toEqual({ route: "connecting", rttMs: null, bitrateKbps: null });
  });

  it("ignores a late stats response after a server switch and stops polling on unmount", async () => {
    const { result, unmount } = renderHook(() => { useScreenShare(); return useRemoteConnectionStats(42); });
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    await signal(5, "direct-offer");
    const pc = Peer.instances[0];
    pc.connectionState = "connected";
    pc.onconnectionstatechange?.();
    let resolve!: (stats: Map<string, unknown>) => void;
    pc.getStats.mockImplementationOnce(() => new Promise(r => { resolve = r; }));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    await act(async () => { useAppStore.setState({ activeServerId: "server-b" }); });
    await act(async () => { resolve(new Map()); });
    expect(result.current.route).toBe("connecting");
    unmount();
    const calls = pc.getStats.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(pc.getStats.mock.calls.length).toBe(calls);
  });

  it("waits for broadcaster admission and queues ICE until the offer arrives", async () => {
    renderHook(() => useScreenShare());
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    expect(send).toHaveBeenCalledWith(42, 8, "", "server-a");
    const pc = Peer.instances[0];
    expect(pc.createOffer).not.toHaveBeenCalled();
    const candidate = { candidate: "candidate:1", sdpMLineIndex: 0 };
    await signal(7, JSON.stringify(candidate));
    expect(pc.addIceCandidate).not.toHaveBeenCalled();
    await signal(5, "direct-offer");
    expect(pc.addIceCandidate).toHaveBeenCalledWith(candidate);
    expect(send).toHaveBeenCalledWith(42, 6, "direct-answer", "server-a");
  });

  it("falls back on decline using the server that owns the viewer after a tab switch", async () => {
    renderHook(() => useScreenShare());
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    await act(async () => { useAppStore.setState({ activeServerId: "server-b" }); });
    await signal(9);
    expect(Peer.instances[0].close).toHaveBeenCalledOnce();
    expect(send).toHaveBeenCalledWith(42, 10, "", "server-a");
    expect(send).toHaveBeenCalledWith(42, 2, "sfu-offer", "server-a");
    expect(send.mock.calls.every((call) => call[3] === "server-a")).toBe(true);
  });

  it("does not send new signal enums to servers without relay support", async () => {
    useAppStore.setState({ serverConfig: { ...useAppStore.getState().serverConfig, webrtc_p2p_relay_available: false } });
    renderHook(() => useScreenShare());
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    expect(send).toHaveBeenCalledWith(42, 2, "sfu-offer", "server-a");
    expect(send.mock.calls.some((call) => call[1] >= 5)).toBe(false);
  });

  it("ignores another server's offer and falls back after the ICE deadline", async () => {
    renderHook(() => useScreenShare());
    await signal(0, JSON.stringify({ native: true, p2p: true, sfuAvailable: true }));
    await signal(5, "wrong-server-offer", "server-b");
    expect(Peer.instances[0].setRemoteDescription).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
    expect(send).toHaveBeenCalledWith(42, 2, "sfu-offer", "server-a");
  });
});

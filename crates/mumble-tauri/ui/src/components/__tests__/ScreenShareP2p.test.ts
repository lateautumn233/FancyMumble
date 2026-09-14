import { act, renderHook, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type SignalHandler = (sender: number, target: number, kind: number, payload: string, server: string) => void;
const signals = vi.hoisted(() => ({ handler: null as SignalHandler | null }));

vi.mock("../../store", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../store")>(),
  onWebRtcSignal: (handler: SignalHandler) => {
    signals.handler = handler;
    return () => { signals.handler = null; };
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn().mockResolvedValue(undefined) }));
vi.mock("../chat/drawing/DrawingOverlay", () => ({
  clearAllStrokesInChannel: vi.fn(), clearStrokesFromSender: vi.fn(),
}));
vi.mock("../chat/stream/useStreamPreview", () => ({
  getPreviewPc: () => null, handlePreviewAnswer: vi.fn(), handlePreviewIceCandidate: vi.fn(),
  clearThumbnail: vi.fn(), closePreview: vi.fn(), storeLocalThumbnail: vi.fn().mockResolvedValue(undefined),
}));

import { useAppStore } from "../../store";
import { useScreenShare } from "../chat/stream/useScreenShare";

class Peer {
  static instances: Peer[] = [];
  connectionState = "new";
  signalingState = "stable";
  remoteDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: (() => void) | null = null;
  onicecandidate: ((event: { candidate: { toJSON: () => RTCIceCandidateInit } }) => void) | null = null;
  close = vi.fn();
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
  await act(async () => { signals.handler?.(42, 1, kind, payload, server); });
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
  useAppStore.setState({ activeServerId: "server-a" });
  await signal(1);
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("native broadcast P2P negotiation", () => {
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

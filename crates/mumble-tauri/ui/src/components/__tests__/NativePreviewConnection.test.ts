import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { connectNativePreview } from "../chat/stream/nativePreviewConnection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), Channel: class { onmessage = vi.fn(); } }));
const status = { serverId: "server-a", broadcastId: "broadcast-a", running: true, encoderId: "h264_nvenc", error: null };
class Peer {
  static latest: Peer;
  remoteDescription: unknown = null;
  connectionState = "new";
  ontrack: ((event: unknown) => void) | null = null;
  onicecandidate: ((event: unknown) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  close = vi.fn();
  addIceCandidate = vi.fn(async () => {});
  setRemoteDescription = vi.fn(async (sdp: unknown) => { this.remoteDescription = sdp; });
  createAnswer = vi.fn(async () => ({ type: "answer", sdp: "answer" }));
  setLocalDescription = vi.fn(async () => {});
  constructor(readonly config: unknown) { Peer.latest = this; }
}
const flush = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
function channel() {
  return (vi.mocked(invoke).mock.calls[0][1] as Record<string, unknown>).channel as { onmessage: (message: unknown) => void };
}
beforeEach(() => {
  vi.mocked(invoke).mockReset().mockResolvedValue(undefined);
  vi.stubGlobal("RTCPeerConnection", Peer);
});
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("native preview connection", () => {
  it("queues early ICE and sends an answer only to local IPC", async () => {
    const stop = connectNativePreview(status, vi.fn(), vi.fn());
    const pc = Peer.latest;
    expect(pc.config).toEqual({ iceServers: [] });
    channel().onmessage({ kind: "ice", payload: '{"candidate":"candidate"}' });
    await flush();
    expect(pc.addIceCandidate).not.toHaveBeenCalled();
    channel().onmessage({ kind: "offer", payload: "offer" });
    await flush();
    expect(pc.addIceCandidate).toHaveBeenCalledWith({ candidate: "candidate" });
    expect(invoke).toHaveBeenCalledWith("native_screen_share_preview", expect.objectContaining({
      serverId: "server-a", broadcastId: "broadcast-a", action: "answer", payload: "answer",
    }));
    expect(vi.mocked(invoke).mock.calls.every(([name]) => name === "native_screen_share_preview")).toBe(true);
    stop();
    await flush();
  });

  it("waits for pending startup before closing and ignores late messages", async () => {
    let resolve!: () => void;
    vi.mocked(invoke).mockImplementationOnce(() => new Promise<void>((r) => { resolve = r; }));
    const onStream = vi.fn();
    const stop = connectNativePreview(status, onStream, vi.fn());
    const oldChannel = channel();
    stop();
    expect(Peer.latest.close).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledTimes(1);
    resolve();
    await flush();
    expect(invoke).toHaveBeenLastCalledWith("native_screen_share_preview", expect.objectContaining({ action: "stop" }));
    oldChannel.onmessage({ kind: "offer", payload: "late" });
    await flush();
    expect(Peer.latest.setRemoteDescription).not.toHaveBeenCalled();
    expect(onStream).not.toHaveBeenCalled();
  });

  it("closes receiver tracks without stopping the broadcast", async () => {
    const track = { stop: vi.fn() };
    const stream = { getTracks: () => [track] };
    const onStream = vi.fn();
    const stop = connectNativePreview(status, onStream, vi.fn());
    Peer.latest.ontrack?.({ streams: [stream] });
    expect(onStream).toHaveBeenCalledWith(stream);
    stop();
    stop();
    await flush();
    expect(track.stop).toHaveBeenCalledOnce();
    expect(Peer.latest.close).toHaveBeenCalledOnce();
    expect(vi.mocked(invoke).mock.calls.filter(([, args]) => (args as Record<string, unknown>)?.action === "stop")).toHaveLength(1);
  });

  it("reports failure and releases the local peer", async () => {
    const onError = vi.fn();
    connectNativePreview(status, vi.fn(), onError);
    channel().onmessage({ kind: "error", payload: "Decoder unavailable" });
    await flush();
    expect(onError).toHaveBeenCalledWith("Error: Decoder unavailable");
    expect(Peer.latest.close).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenLastCalledWith("native_screen_share_preview", expect.objectContaining({ action: "stop" }));
  });

  it("times out a stalled preview", async () => {
    vi.useFakeTimers();
    const onError = vi.fn();
    connectNativePreview(status, vi.fn(), onError);
    await vi.advanceTimersByTimeAsync(20_000);
    expect(onError).toHaveBeenCalledWith("Preview connection timed out");
    expect(Peer.latest.close).toHaveBeenCalledOnce();
  });
});

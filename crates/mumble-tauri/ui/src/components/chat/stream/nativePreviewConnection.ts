import { Channel, invoke } from "@tauri-apps/api/core";
import type { NativeStatus } from "./nativeBroadcast";

interface PreviewSignal { kind: "offer" | "ice" | "error"; payload: string }

/** Receive the existing encoder output locally, without server signaling. */
export function connectNativePreview(status: NativeStatus, onStream: (stream: MediaStream) => void,
  onError: (error: string) => void): () => void {
  const pc = new RTCPeerConnection({ iceServers: [] });
  const args = { serverId: status.serverId, broadcastId: status.broadcastId, previewId: crypto.randomUUID() };
  let disposed = false;
  let stream: MediaStream | null = null;
  let started: Promise<void>;
  const pendingIce: RTCIceCandidateInit[] = [];
  const send = (action: string, payload?: string) =>
    invoke<void>("native_screen_share_preview", { ...args, action, payload, channel });
  const timer = setTimeout(() => fail("Preview connection timed out"), 20_000);
  function dispose() {
    if (disposed) return;
    disposed = true;
    clearTimeout(timer);
    pc.ontrack = null;
    pc.onicecandidate = null;
    pc.onconnectionstatechange = null;
    pc.close();
    stream?.getTracks().forEach((track) => track.stop());
    void started.then(() => send("stop")).catch(() => {});
  }
  function fail(error: unknown) {
    if (disposed) return;
    onError(String(error));
    dispose();
  }
  pc.ontrack = (event) => {
    if (disposed) return;
    stream = event.streams[0] ?? new MediaStream([event.track]);
    onStream(stream);
  };
  pc.onicecandidate = (event) => {
    if (event.candidate && !disposed) {
      const payload = JSON.stringify(event.candidate.toJSON());
      void started.then(() => { if (!disposed) return send("ice", payload); }).catch(fail);
    }
  };
  pc.onconnectionstatechange = () => {
    if (pc.connectionState === "connected") clearTimeout(timer);
    if (["failed", "closed", "disconnected"].includes(pc.connectionState)) fail("Preview connection lost");
  };
  // Preserve offer/answer/ICE order across asynchronous IPC callbacks.
  let incoming = Promise.resolve();
  const channel = new Channel<PreviewSignal>();
  channel.onmessage = (message) => {
    incoming = incoming.then(async () => {
      if (disposed) return;
      if (message.kind === "error") throw new Error(message.payload);
      if (message.kind === "ice") {
        const candidate = JSON.parse(message.payload) as RTCIceCandidateInit;
        if (pc.remoteDescription) await pc.addIceCandidate(candidate);
        else if (pendingIce.length < 64) pendingIce.push(candidate);
        else throw new Error("Too many preview ICE candidates");
        return;
      }
      await pc.setRemoteDescription({ type: "offer", sdp: message.payload });
      if (disposed) return;
      for (const candidate of pendingIce.splice(0)) await pc.addIceCandidate(candidate);
      const answer = await pc.createAnswer();
      if (disposed) return;
      await pc.setLocalDescription(answer);
      await started;
      if (!disposed) await send("answer", answer.sdp);
    }).catch(fail);
  };
  started = invoke<void>("native_screen_share_preview", { ...args, action: "start", channel });
  void started.catch(fail);
  return dispose;
}

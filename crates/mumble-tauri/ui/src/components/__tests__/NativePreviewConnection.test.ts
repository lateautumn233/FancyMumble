import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { fetchNativePreviewFrame } from "../chat/stream/nativePreviewConnection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("native preview frame", () => {
  it("requests a JPEG snapshot through Tauri IPC without WebRTC", async () => {
    vi.mocked(invoke).mockResolvedValue({ sequence: 3, width: 640, height: 360, mime: "image/jpeg", data: "abc" });
    const status = { serverId: "server-a", broadcastId: "broadcast-a", running: true, encoderId: "h264_nvenc", error: null };
    await expect(fetchNativePreviewFrame(status)).resolves.toMatchObject({ sequence: 3, mime: "image/jpeg" });
    expect(invoke).toHaveBeenCalledWith("native_screen_share_preview_frame", {
      serverId: "server-a", broadcastId: "broadcast-a", maxWidth: 640,
    });
    expect(globalThis.RTCPeerConnection).toBeUndefined();
  });
});

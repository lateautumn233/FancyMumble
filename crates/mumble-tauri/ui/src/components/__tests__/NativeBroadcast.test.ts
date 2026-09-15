import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../store";
import { restoreNativeBroadcast, startNativeBroadcast, stopNativeBroadcast, useNativeBroadcast, type NativeStatus } from "../chat/stream/nativeBroadcast";
import type { NativeShareRequest } from "../chat/stream/nativeSettings";

const events = vi.hoisted(() => ({ handler: null as ((event: { payload: NativeStatus }) => void) | null }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (_name, handler) => { events.handler = handler; return vi.fn(); }) }));
vi.mock("../chat/drawing/DrawingOverlay", () => ({ clearAllStrokesInChannel: vi.fn() }));

const context = { serverId: "a", ownSession: 1, channelId: 0 };
const status: NativeStatus = { serverId: "a", broadcastId: "new", running: true, encoderId: "h264_nvenc", error: null };
const request: NativeShareRequest = { source: { kind: "monitor", outputIndex: 0, hmonitor: "1" }, drawCursor: true,
  settings: { capture: "ddagrab", encoder: "auto", resolution: { mode: "native" }, fps: 30, bitrateKbps: null, p2p: "auto", p2pMaxViewers: 2 } };
beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useNativeBroadcast.setState({ broadcast: null, stopping: false });
  useAppStore.setState({ activeServerId: "a", ownSession: 1, currentChannel: 0, isSharingOwn: false, broadcastingOwnSession: null, broadcastingSessions: new Set(), webrtcError: null });
  vi.mocked(invoke).mockResolvedValue(status);
});

describe("native broadcast lifecycle", () => {
  it("does not resurrect a broadcast that stops while restoring its status", async () => {
    vi.mocked(invoke).mockImplementation(async () => {
      events.handler?.({ payload: { ...status, running: false, error: null } });
      return status;
    });
    await restoreNativeBroadcast(context);
    expect(useNativeBroadcast.getState().broadcast).toBeNull();
    expect(useAppStore.getState().isSharingOwn).toBe(false);
  });

  it("stops on the owning server after switching to another tab with the same session id", async () => {
    await startNativeBroadcast(request, context, "Display 1");
    useAppStore.setState({ activeServerId: "b", ownSession: 1 });
    await stopNativeBroadcast();
    expect(invoke).toHaveBeenCalledWith("stop_native_screen_share", { serverId: "a" });
    expect(useNativeBroadcast.getState().broadcast).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("send_webrtc_signal", expect.anything());
  });

  it("ignores stale events and handles source failure even before the start response", async () => {
    vi.mocked(invoke).mockImplementation(async () => {
      events.handler?.({ payload: { ...status, running: false, error: "Source closed" } });
      return status;
    });
    await startNativeBroadcast(request, context, "Display 1");
    expect(useNativeBroadcast.getState().broadcast).toBeNull();
    expect(useAppStore.getState().webrtcError).toBe("Source closed");
    vi.mocked(invoke).mockResolvedValue(status);
    await startNativeBroadcast(request, context, "Display 1");
    events.handler?.({ payload: { ...status, broadcastId: "old", running: false } });
    expect(useNativeBroadcast.getState().broadcast?.status?.broadcastId).toBe("new");
  });

  it("releases the reservation after a failed start so retry works", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("Capture failed"));
    await expect(startNativeBroadcast(request, context, "Display 1")).rejects.toThrow("Capture failed");
    expect(useNativeBroadcast.getState().broadcast).toBeNull();
    await startNativeBroadcast(request, context, "Display 1");
    expect(useAppStore.getState().isSharingOwn).toBe(true);
  });
});

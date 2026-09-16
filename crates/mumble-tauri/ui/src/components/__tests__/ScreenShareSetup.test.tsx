import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../store";
import { ScreenShareSetupDialog } from "../chat/stream/ScreenShareSetupDialog";
import type { SharePreferences } from "../chat/stream/nativeSettings";

const storage = vi.hoisted(() => ({ get: vi.fn(), set: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../utils/store", () => ({ load: vi.fn(async () => storage) }));

const defaults: SharePreferences = {
  settings: { capture: "ddagrab", encoder: "auto", resolution: { mode: "native" }, fps: 30, bitrateKbps: null, p2p: "auto", p2pMaxViewers: 2 },
  drawCursor: true,
  shareAudio: true,
  viewerStats: { enabled: true, connection: true, video: true, network: true },
};
const context = { serverId: "server-a", ownSession: 1, channelId: 0 };
const start = vi.fn().mockResolvedValue(undefined);
const close = vi.fn();
beforeEach(() => {
  vi.clearAllMocks();
  storage.get.mockResolvedValue(null);
  storage.set.mockResolvedValue(undefined);
  storage.save.mockResolvedValue(undefined);
  useAppStore.setState({ activeServerId: context.serverId, ownSession: 1, currentChannel: 0,
    serverConfig: { ...useAppStore.getState().serverConfig, webrtc_sfu_available: true } });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "default_screen_share_settings") return structuredClone(defaults.settings);
    if (command === "validate_screen_share_settings") return (args as { settings: unknown }).settings;
    if (command === "list_video_encoders") return { supported: true, autoSelected: "h264_nvenc", encoders: [
      { id: "h264_nvenc", displayName: "NVIDIA H.264", available: true, detail: null },
      { id: "h264_amf", displayName: "AMD H.264", available: false, detail: "Driver unavailable" },
    ] };
    if (command === "list_screen_share_sources") return [
      { id: "monitor:1", name: "Display 1", size: { width: 1920, height: 1080 }, source: { kind: "monitor", outputIndex: 0, hmonitor: "1" } },
      { id: "window:1", name: "Editor", size: { width: 1200, height: 800 }, source: { kind: "window", hwnd: "18446744073709551615" } },
    ];
    if (command === "resolve_screen_share_encoding") return { encoding: { size: { width: 1920, height: 1080 }, fps: 30, bitrateKbps: 4354 }, encoder: { id: "h264_nvenc", fellBack: false } };
    if (command === "suggested_screen_share_bitrate") return 4354;
    throw new Error(command);
  });
});
afterEach(cleanup);

describe("screen-share preflight", () => {
  it("enables audio for existing preferences and preserves an explicit opt-out", async () => {
    storage.get.mockResolvedValue({ settings: defaults.settings, drawCursor: true });
    const { unmount } = render(<ScreenShareSetupDialog context={context} onClose={close} onStart={start} />);
    expect((await screen.findByRole("checkbox", { name: "Share audio" }) as HTMLInputElement).checked).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "Share audio" }));
    fireEvent.click(screen.getByRole("button", { name: "Set as default" }));
    await screen.findByRole("button", { name: "Defaults saved" });
    expect(storage.set).toHaveBeenCalledWith("screenShare", expect.objectContaining({ shareAudio: false }));
    unmount();
    storage.get.mockResolvedValue({ ...defaults, shareAudio: false });
    render(<ScreenShareSetupDialog context={context} onClose={close} onStart={start} />);
    expect((await screen.findByRole("checkbox", { name: "Share audio" }) as HTMLInputElement).checked).toBe(false);
    fireEvent.click(screen.getByRole("radio", { name: /Display 1/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Start sharing" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Start sharing" }));
    await waitFor(() => expect(start).toHaveBeenCalled());
    expect(start.mock.calls[0][0].shareAudio).toBe(false);
  });

  it("starts with temporary common settings without overwriting defaults", async () => {
    render(<ScreenShareSetupDialog context={context} onClose={close} onStart={start} />);
    fireEvent.click(await screen.findByRole("radio", { name: /Display 1/ }));
    fireEvent.change(screen.getByLabelText("Frame rate"), { target: { value: "60" } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Start sharing" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Start sharing" }));
    await waitFor(() => expect(start).toHaveBeenCalled());
    expect(start.mock.calls[0][0].settings.fps).toBe(60);
    expect(start.mock.calls[0][0].shareAudio).toBe(true);
    expect(start.mock.calls[0][1]).toEqual(context);
    expect(storage.set).not.toHaveBeenCalled();
  });

  it("saves defaults only on explicit request and disables unavailable encoders", async () => {
    render(<ScreenShareSetupDialog context={context} onClose={close} onStart={start} />);
    await screen.findByLabelText("Encoder");
    expect((screen.getByRole("option", { name: /AMD H.264/ }) as HTMLOptionElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Resolution"), { target: { value: "720" } });
    fireEvent.click(screen.getByRole("button", { name: "Set as default" }));
    await screen.findByRole("button", { name: "Defaults saved" });
    expect(storage.set).toHaveBeenCalledWith("screenShare", expect.objectContaining({ settings: expect.objectContaining({ resolution: { mode: "preset", lines: 720 } }) }));
    expect(start).not.toHaveBeenCalled();
  });

  it("filters windows by backend and preserves the full HWND", async () => {
    render(<ScreenShareSetupDialog context={context} onClose={close} onStart={start} />);
    await screen.findByLabelText("Encoder");
    expect((screen.getByRole("tab", { name: "Windows" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Advanced" }));
    fireEvent.change(screen.getByLabelText("Capture backend"), { target: { value: "gfxcapture" } });
    fireEvent.click(screen.getByRole("tab", { name: "Windows" }));
    fireEvent.click(screen.getByRole("radio", { name: /Editor/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Start sharing" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Start sharing" }));
    await waitFor(() => expect(start).toHaveBeenCalled());
    expect(start.mock.calls[0][0].source).toEqual({ kind: "window", hwnd: "18446744073709551615" });
  });

  it("blocks start after switching servers even when session numbers match", async () => {
    render(<ScreenShareSetupDialog context={context} onClose={close} onStart={start} />);
    fireEvent.click(await screen.findByRole("radio", { name: /Display 1/ }));
    await act(async () => { useAppStore.setState({ activeServerId: "server-b" }); });
    expect((screen.getByRole("button", { name: "Start sharing" }) as HTMLButtonElement).disabled).toBe(true);
    expect(start).not.toHaveBeenCalled();
  });
});

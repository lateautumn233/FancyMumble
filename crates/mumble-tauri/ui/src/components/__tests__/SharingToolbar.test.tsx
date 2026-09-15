import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../store";
import { useNativeBroadcast } from "../chat/stream/nativeBroadcast";
import { SharingToolbar } from "../chat/stream/SharingToolbar";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../chat/drawing/DrawingOverlay", () => ({ clearAllStrokesInChannel: vi.fn() }));
beforeEach(() => {
  vi.mocked(invoke).mockReset().mockResolvedValue(undefined);
  useAppStore.setState({ isSharingOwn: true, activeServerId: "other-server", broadcastingSessions: new Set() });
  useNativeBroadcast.setState({ stopping: false, broadcast: {
    serverId: "own-server", ownSession: 1, channelId: 0, sourceName: "Display 1",
    status: { serverId: "own-server", broadcastId: "broadcast", running: true, encoderId: "h264_nvenc", error: null },
  } });
});
describe("sharing toolbar", () => {
  it("stops the owning broadcast even with hidden preview and a different active tab", async () => {
    render(<SharingToolbar onStop={vi.fn()} previewVisible={false} onPreviewChange={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Stop sharing" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("stop_native_screen_share", { serverId: "own-server" }));
    await waitFor(() => expect(useNativeBroadcast.getState().broadcast).toBeNull());
  });
  it("collapses only the preview", () => {
    const onChange = vi.fn();
    render(<SharingToolbar onStop={vi.fn()} previewVisible onPreviewChange={onChange} />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Preview" }));
    expect(onChange).toHaveBeenCalledWith(false);
    expect(invoke).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Stop sharing" })).toBeTruthy();
  });
  it("keeps the stop action available for retry after failure", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("Stop failed"));
    render(<SharingToolbar onStop={vi.fn()} previewVisible onPreviewChange={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Stop sharing" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Stop failed"));
    expect(useNativeBroadcast.getState().stopping).toBe(false);
    expect(useNativeBroadcast.getState().broadcast).not.toBeNull();
  });
});

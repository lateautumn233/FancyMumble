import { invoke } from "@tauri-apps/api/core";
import type { NativeStatus } from "./nativeBroadcast";

export interface NativePreviewFrame {
  sequence: number;
  width: number;
  height: number;
  mime: string;
  data: string;
}

/** Fetch one native preview image without creating a WebRTC peer. */
export function fetchNativePreviewFrame(
  status: NativeStatus,
  maxWidth = 640,
): Promise<NativePreviewFrame | null> {
  return invoke<NativePreviewFrame | null>("native_screen_share_preview_frame", {
    serverId: status.serverId,
    broadcastId: status.broadcastId,
    maxWidth,
  });
}

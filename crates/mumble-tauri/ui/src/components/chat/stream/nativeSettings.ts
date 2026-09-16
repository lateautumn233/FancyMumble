import { invoke } from "@tauri-apps/api/core";
import { load } from "../../../utils/store";

export interface ScreenShareSettings {
  capture: "ddagrab" | "gfxcapture";
  encoder: string;
  resolution: { mode: "native" } | { mode: "preset"; lines: number } | { mode: "custom"; width: number; height: number };
  fps: number;
  bitrateKbps: number | null;
  p2p: "auto" | "disabled";
  p2pMaxViewers: number;
}
export interface ViewerStatsPreferences {
  enabled: boolean;
  connection: boolean;
  video: boolean;
  network: boolean;
}
export const DEFAULT_VIEWER_STATS_PREFERENCES: ViewerStatsPreferences = {
  enabled: true,
  connection: true,
  video: true,
  network: true,
};
export interface SharePreferences {
  settings: ScreenShareSettings;
  drawCursor: boolean;
  shareAudio: boolean;
  viewerStats: ViewerStatsPreferences;
}
export interface EncoderReport {
  supported: boolean;
  autoSelected: string | null;
  encoders: { id: string; displayName: string; available: boolean; detail: string | null; codec: string }[];
}
export interface CaptureSourceInfo {
  id: string;
  name: string;
  size: { width: number; height: number };
  source: { kind: "monitor"; outputIndex: number; hmonitor: string } | { kind: "window"; hwnd: string };
}
export interface ResolvedShare {
  encoding: { size: { width: number; height: number }; fps: number; bitrateKbps: number };
  encoder: { id: string; fellBack: boolean };
}
export type NativeShareRequest = Omit<SharePreferences, "viewerStats"> & {
  source: CaptureSourceInfo["source"];
};

function normalizeViewerStatsPreferences(value?: Partial<ViewerStatsPreferences>): ViewerStatsPreferences {
  return {
    enabled: value?.enabled !== false,
    connection: value?.connection !== false,
    video: value?.video !== false,
    network: value?.network !== false,
  };
}

export async function loadViewerStatsPreferences(): Promise<ViewerStatsPreferences> {
  const store = await load("preferences.json", { autoSave: true, defaults: {} });
  const saved = await store.get<{ viewerStats?: Partial<ViewerStatsPreferences> }>("screenShare");
  return normalizeViewerStatsPreferences(saved?.viewerStats);
}

export async function loadSharePreferences(): Promise<SharePreferences> {
  const defaults = await invoke<ScreenShareSettings>("default_screen_share_settings");
  const store = await load("preferences.json", { autoSave: true, defaults: {} });
  const saved = await store.get<Partial<SharePreferences>>("screenShare");
  const settings = await invoke<ScreenShareSettings>("validate_screen_share_settings", {
    settings: { ...defaults, ...saved?.settings },
  });
  return {
    settings,
    drawCursor: saved?.drawCursor !== false,
    shareAudio: saved?.shareAudio !== false,
    viewerStats: normalizeViewerStatsPreferences(saved?.viewerStats),
  };
}

export async function saveSharePreferences(value: SharePreferences): Promise<void> {
  const settings = await invoke<ScreenShareSettings>("validate_screen_share_settings", { settings: value.settings });
  const store = await load("preferences.json", { autoSave: true, defaults: {} });
  await store.set("screenShare", {
    settings,
    drawCursor: value.drawCursor,
    shareAudio: value.shareAudio,
    viewerStats: normalizeViewerStatsPreferences(value.viewerStats),
  });
  await store.save();
}

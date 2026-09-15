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
export interface SharePreferences { settings: ScreenShareSettings; drawCursor: boolean }
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
export interface NativeShareRequest extends SharePreferences { source: CaptureSourceInfo["source"] }

export async function loadSharePreferences(): Promise<SharePreferences> {
  const defaults = await invoke<ScreenShareSettings>("default_screen_share_settings");
  const store = await load("preferences.json", { autoSave: true, defaults: {} });
  const saved = await store.get<Partial<SharePreferences>>("screenShare");
  const settings = await invoke<ScreenShareSettings>("validate_screen_share_settings", {
    settings: { ...defaults, ...saved?.settings },
  });
  return { settings, drawCursor: saved?.drawCursor !== false };
}

export async function saveSharePreferences(value: SharePreferences): Promise<void> {
  const settings = await invoke<ScreenShareSettings>("validate_screen_share_settings", { settings: value.settings });
  const store = await load("preferences.json", { autoSave: true, defaults: {} });
  await store.set("screenShare", { settings, drawCursor: value.drawCursor });
  await store.save();
}

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { useAppStore } from "../../../store";
import { clearAllStrokesInChannel } from "../drawing/DrawingOverlay";
import type { NativeShareRequest } from "./nativeSettings";

export interface NativeStatus {
  serverId: string;
  broadcastId: string;
  running: boolean;
  encoderId: string | null;
  error: string | null;
}
export interface ShareContext { serverId: string; ownSession: number; channelId: number }
interface Broadcast extends ShareContext { status: NativeStatus | null; sourceName: string }
export const useNativeBroadcast = create<{ broadcast: Broadcast | null; stopping: boolean }>(() => ({ broadcast: null, stopping: false }));

let listening: Promise<unknown> | null = null;
const earlyEvents = new Map<string, NativeStatus>();

function acceptStatus(status: NativeStatus): void {
  const current = useNativeBroadcast.getState().broadcast;
  if (!current || current.serverId !== status.serverId) return;
  if (!current.status) {
    if (earlyEvents.size >= 16) earlyEvents.clear();
    earlyEvents.set(status.broadcastId, status);
    return;
  }
  if (current.status.broadcastId !== status.broadcastId) return;
  if (status.running) {
    useNativeBroadcast.setState({ broadcast: { ...current, status } });
    return;
  }
  useNativeBroadcast.setState({ broadcast: null, stopping: false });
  clearAllStrokesInChannel(current.channelId);
  const state = useAppStore.getState();
  if (state.desktopDrawingOverlayOpen) void invoke("close_drawing_overlay").catch(() => {});
  useAppStore.setState((s) => {
    const sessions = new Set(s.broadcastingSessions);
    if (s.activeServerId === current.serverId) sessions.delete(current.ownSession);
    return { isSharingOwn: false, broadcastingOwnSession: null, broadcastingSessions: sessions,
      desktopDrawingOverlayOpen: false, webrtcConnecting: false,
      ...(status.error ? { webrtcError: status.error } : {}) };
  });
}

function ensureListener(): Promise<unknown> {
  listening ??= listen<NativeStatus>("native-screen-share-state", ({ payload }) => acceptStatus(payload)).catch((error: unknown) => {
    listening = null;
    throw error;
  });
  return listening;
}

function publishStarted(broadcast: Broadcast): void {
  useNativeBroadcast.setState({ broadcast });
  useAppStore.setState((s) => {
    const sessions = new Set(s.broadcastingSessions);
    if (s.activeServerId === broadcast.serverId) sessions.add(broadcast.ownSession);
    return { isSharingOwn: true, broadcastingOwnSession: broadcast.ownSession, broadcastingSessions: sessions };
  });
}

export async function restoreNativeBroadcast(context: ShareContext): Promise<void> {
  await ensureListener();
  if (useNativeBroadcast.getState().broadcast) return;
  const pending: Broadcast = { ...context, status: null, sourceName: "" };
  useNativeBroadcast.setState({ broadcast: pending });
  try {
    const status = await invoke<NativeStatus | null>("native_screen_share_status", { serverId: context.serverId });
    if (status?.running) {
      publishStarted({ ...pending, status });
      const latest = earlyEvents.get(status.broadcastId);
      if (latest) acceptStatus(latest);
    } else {
      useNativeBroadcast.setState({ broadcast: null });
    }
  } catch (error) {
    if (useNativeBroadcast.getState().broadcast === pending) useNativeBroadcast.setState({ broadcast: null });
    throw error;
  } finally {
    earlyEvents.clear();
  }
}

export async function startNativeBroadcast(request: NativeShareRequest, context: ShareContext, sourceName: string): Promise<void> {
  if (useNativeBroadcast.getState().broadcast || useAppStore.getState().isSharingOwn) throw new Error("A screen share is already active");
  const pending: Broadcast = { ...context, status: null, sourceName };
  useNativeBroadcast.setState({ broadcast: pending });
  try {
    await ensureListener();
    const state = useAppStore.getState();
    if (state.activeServerId !== context.serverId || state.currentChannel !== context.channelId || state.ownSession !== context.ownSession) {
      throw new Error("The server or channel changed before sharing started");
    }
    const status = await invoke<NativeStatus>("start_native_screen_share", { request, serverId: context.serverId });
    publishStarted({ ...pending, status });
    const latest = earlyEvents.get(status.broadcastId);
    earlyEvents.clear();
    if (latest) acceptStatus(latest);
  } catch (error) {
    if (useNativeBroadcast.getState().broadcast === pending) useNativeBroadcast.setState({ broadcast: null });
    earlyEvents.clear();
    throw error;
  }
}

export async function stopNativeBroadcast(): Promise<void> {
  const current = useNativeBroadcast.getState().broadcast;
  if (!current?.status || useNativeBroadcast.getState().stopping) return;
  useNativeBroadcast.setState({ stopping: true });
  try {
    await invoke("stop_native_screen_share", { serverId: current.serverId });
    acceptStatus({ ...current.status, running: false, error: null });
  } finally {
    useNativeBroadcast.setState({ stopping: false });
  }
}

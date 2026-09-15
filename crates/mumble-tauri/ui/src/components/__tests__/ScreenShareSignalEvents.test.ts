import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UserEntry } from "../../types";

const mocks = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: unknown }) => unknown>(),
  invoke: vi.fn(),
  preferences: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => unknown) => {
    mocks.listeners.set(name, handler);
    return () => { mocks.listeners.delete(name); };
  }),
  emit: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("../../preferencesStorage", () => ({ getPreferences: mocks.preferences }));
vi.mock("../chat/drawing/DrawingOverlay", () => ({ clearStrokesFromSender: vi.fn() }));

import { initEventListeners, onWebRtcSignal, useAppStore } from "../../store";
import { announcedBroadcasts, forgetBroadcasts } from "../chat/stream/screenShareSignals";

let cleanup: Array<() => void> = [];
function emit(kind: number, serverId = "a") {
  mocks.listeners.get("webrtc-signal")!({ payload: {
    sender_session: 42, target_session: 0, signal_type: kind, payload: "native-payload", serverId,
  } });
}

beforeEach(() => {
  vi.useFakeTimers();
  mocks.listeners.clear();
  mocks.invoke.mockReset().mockResolvedValue([]);
  mocks.preferences.mockReset().mockResolvedValue({});
  useAppStore.setState({
    activeServerId: "a", users: [], sessions: [], pendingConnect: null,
    broadcastingSessions: new Set(), channelPersistence: {},
    refreshSessions: vi.fn().mockResolvedValue(undefined),
  });
});

afterEach(() => {
  for (const stop of cleanup.splice(0)) stop();
  forgetBroadcasts("a");
  forgetBroadcasts("b");
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("global screen share signal events", () => {
  it("captures START while asynchronous app bootstrap is still pending", async () => {
    let resolve!: (value: object) => void;
    mocks.preferences.mockReturnValue(new Promise(r => { resolve = r; }));
    const initializing = initEventListeners(vi.fn());
    expect([...mocks.listeners.keys()][0]).toBe("webrtc-signal");
    emit(0);
    expect(announcedBroadcasts("a")).toEqual([42]);
    resolve({});
    cleanup.push(...await initializing);
    const handler = vi.fn();
    cleanup.push(onWebRtcSignal(handler, "a"));
    expect(handler).toHaveBeenCalledWith(42, 0, 0, "native-payload", "a");
  });

  it("does not replay a broadcast stopped before the UI subscribes", async () => {
    cleanup.push(...await initEventListeners(vi.fn()));
    emit(0);
    emit(1);
    const handler = vi.fn();
    cleanup.push(onWebRtcSignal(handler, "a"));
    expect(handler).not.toHaveBeenCalled();
  });

  it("clears a disconnected background server without clearing the active server", async () => {
    cleanup.push(...await initEventListeners(vi.fn()));
    emit(0, "a");
    emit(0, "b");
    await mocks.listeners.get("server-disconnected")!({ payload: { serverId: "b", reason: null } });
    expect(announcedBroadcasts("b")).toEqual([]);
    expect(announcedBroadcasts("a")).toEqual([42]);
  });

  it("removes departed users from the cache even when no chat view is mounted", async () => {
    cleanup.push(...await initEventListeners(vi.fn()));
    useAppStore.setState({ users: [{ session: 42, channel_id: 0 } as UserEntry] });
    emit(0);
    await useAppStore.getState().refreshState();
    expect(announcedBroadcasts("a")).toEqual([]);
  });

  it("does not discard an early START just because the initial user snapshot is still empty", async () => {
    cleanup.push(...await initEventListeners(vi.fn()));
    emit(0);
    await useAppStore.getState().refreshState();
    expect(announcedBroadcasts("a")).toEqual([42]);
  });
});

export type WebRtcSignalHandler = (
  sender: number | null, target: number | null, kind: number, payload: string, serverId: string | null,
) => void;
type Signal = Parameters<WebRtcSignalHandler>;

const START = 0;
const STOP = 1;
const handlers = new Set<WebRtcSignalHandler>();
const announcements = new Map<string | null, Map<number, Signal>>();

/** Retain only current announcements, never SDP/ICE from an old connection. */
export function dispatchWebRtcSignal(...signal: Signal): void {
  const [sender, , kind, , serverId] = signal;
  if (sender !== null) {
    if (kind === START) {
      let broadcasts = announcements.get(serverId);
      if (!broadcasts) announcements.set(serverId, broadcasts = new Map());
      broadcasts.set(sender, signal);
    } else if (kind === STOP) {
      const broadcasts = announcements.get(serverId);
      broadcasts?.delete(sender);
      if (broadcasts?.size === 0) announcements.delete(serverId);
    }
  }
  for (const handler of handlers) handler(...signal);
}

export function announcedBroadcasts(serverId: string | null): number[] {
  return [...announcements.get(serverId)?.keys() ?? []];
}

/** A chat view can mount after START or after its server becomes active. */
export function onWebRtcSignal(handler: WebRtcSignalHandler, replayServerId?: string | null): () => void {
  handlers.add(handler);
  for (const [serverId, broadcasts] of announcements) {
    if (replayServerId !== undefined && replayServerId !== serverId) continue;
    for (const signal of broadcasts.values()) handler(...signal);
  }
  return () => { handlers.delete(handler); };
}

export function forgetBroadcasts(serverId: string | null, sessions = announcedBroadcasts(serverId)): void {
  for (const session of sessions) {
    if (announcements.get(serverId)?.has(session)) {
      dispatchWebRtcSignal(session, null, STOP, "", serverId);
    }
  }
}

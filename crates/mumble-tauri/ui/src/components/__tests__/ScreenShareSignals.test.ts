import { afterEach, describe, expect, it, vi } from "vitest";
import { announcedBroadcasts, dispatchWebRtcSignal, forgetBroadcasts, onWebRtcSignal } from "../chat/stream/screenShareSignals";

const unsubscribe: Array<() => void> = [];
afterEach(() => {
  for (const stop of unsubscribe.splice(0)) stop();
  for (const server of ["a", "b", null]) forgetBroadcasts(server);
});

describe("screen share announcement cache", () => {
  it("replays only the latest START and never old SDP or ICE", () => {
    dispatchWebRtcSignal(42, 0, 0, "old", "a");
    dispatchWebRtcSignal(42, 0, 0, "latest", "a");
    dispatchWebRtcSignal(42, 1, 5, "offer", "a");
    dispatchWebRtcSignal(42, 1, 7, "candidate", "a");
    const handler = vi.fn();
    unsubscribe.push(onWebRtcSignal(handler, "a"));
    expect(handler.mock.calls).toEqual([[42, 0, 0, "latest", "a"]]);
  });

  it("isolates identical session IDs across servers and clears only the disconnected server", () => {
    dispatchWebRtcSignal(42, 0, 0, "a", "a");
    dispatchWebRtcSignal(42, 0, 0, "b", "b");
    forgetBroadcasts("a");
    const handler = vi.fn();
    unsubscribe.push(onWebRtcSignal(handler));
    expect(handler.mock.calls).toEqual([[42, 0, 0, "b", "b"]]);
    expect(announcedBroadcasts("a")).toEqual([]);
  });

  it("forgets departing broadcasters and notifies mounted viewers", () => {
    dispatchWebRtcSignal(42, 0, 0, "", "a");
    dispatchWebRtcSignal(43, 0, 0, "", "a");
    const handler = vi.fn();
    unsubscribe.push(onWebRtcSignal(handler, "a"));
    handler.mockClear();
    forgetBroadcasts("a", [42]);
    expect(handler.mock.calls).toEqual([[42, null, 1, "", "a"]]);
    expect(announcedBroadcasts("a")).toEqual([43]);
  });

  it("does not replay a STOP or deliver signals to an unmounted subscriber", () => {
    const handler = vi.fn();
    const stop = onWebRtcSignal(handler, "a");
    stop();
    dispatchWebRtcSignal(42, 0, 0, "", "a");
    dispatchWebRtcSignal(42, 0, 1, "", "a");
    unsubscribe.push(onWebRtcSignal(handler, "a"));
    expect(handler).not.toHaveBeenCalled();
  });

  it("keeps unknown-server signals separate and ignores an absent sender", () => {
    dispatchWebRtcSignal(42, 0, 0, "", null);
    dispatchWebRtcSignal(null, 0, 0, "", "a");
    const handler = vi.fn();
    unsubscribe.push(onWebRtcSignal(handler, "a"));
    expect(handler).not.toHaveBeenCalled();
    expect(announcedBroadcasts(null)).toEqual([42]);
  });
});

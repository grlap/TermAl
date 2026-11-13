// Pins the liveness judgement for an OPEN live event stream
// (live-stream-liveness.ts): silent only after heartbeats were seen and no
// event of any kind arrived for LIVE_STREAM_SILENCE_RESTART_MS.
import { describe, expect, it } from "vitest";
import { LiveStreamLiveness } from "./live-stream-liveness";
import { LIVE_STREAM_SILENCE_RESTART_MS } from "./shared-live-events";

function setup() {
  let now = 1_000;
  const liveness = new LiveStreamLiveness(() => now);
  const source = new EventTarget();
  const detach = liveness.attach(source);
  return {
    liveness,
    detach,
    advance: (ms: number) => {
      now += ms;
    },
    emit: (type: string) => source.dispatchEvent(new Event(type)),
  };
}

describe("LiveStreamLiveness", () => {
  it("is never silent before the stream has sent a heartbeat", () => {
    const { liveness, advance, emit } = setup();
    emit("state");
    advance(LIVE_STREAM_SILENCE_RESTART_MS * 10);
    expect(liveness.isSilent()).toBe(false);
  });

  it("becomes silent once heartbeats stop for the limit", () => {
    const { liveness, advance, emit } = setup();
    emit("heartbeat");
    advance(LIVE_STREAM_SILENCE_RESTART_MS - 1);
    expect(liveness.isSilent()).toBe(false);
    advance(1);
    expect(liveness.isSilent()).toBe(true);
  });

  it("counts any live event, and the open itself, as activity", () => {
    const { liveness, advance, emit } = setup();
    emit("heartbeat");
    advance(LIVE_STREAM_SILENCE_RESTART_MS - 1);
    emit("delta");
    advance(LIVE_STREAM_SILENCE_RESTART_MS - 1);
    expect(liveness.isSilent()).toBe(false);
    liveness.markOpen();
    advance(LIVE_STREAM_SILENCE_RESTART_MS - 1);
    expect(liveness.isSilent()).toBe(false);
  });

  it("stops listening once detached", () => {
    const { liveness, detach, advance, emit } = setup();
    detach();
    emit("heartbeat");
    advance(LIVE_STREAM_SILENCE_RESTART_MS * 2);
    expect(liveness.isSilent()).toBe(false);
  });
});

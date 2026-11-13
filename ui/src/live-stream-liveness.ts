// live-stream-liveness.ts
//
// Owns: judging whether an OPEN live event stream has gone silent. It records
// when the stream last delivered any named event (or opened) and whether it
// has ever delivered a server `heartbeat`. A stream that proved it sends
// heartbeats and then delivers nothing for LIVE_STREAM_SILENCE_RESTART_MS is
// silent; one that never sent a heartbeat (an older backend) never is, so a
// quiet stream from such a backend is not churned.
//
// Does not own: replacing the stream (the transport's recovery in
// app-live-state-transport.ts), the SharedWorker hub's own upstream check
// (shared-live-events.ts), or the event payloads.
//
// New module, not split from an existing file. It keeps this bookkeeping out
// of app-live-state-transport.ts, which is already large.

import { LIVE_EVENT_TYPES, LIVE_STREAM_SILENCE_RESTART_MS } from "./shared-live-events";

type ListenerTarget = Pick<EventTarget, "addEventListener" | "removeEventListener">;

export class LiveStreamLiveness {
  private lastEventAt: number;
  private sendsHeartbeats = false;

  constructor(private readonly now: () => number = () => Date.now()) {
    this.lastEventAt = now();
  }

  /** Listens for every live event type on `source`; returns the detach. */
  attach(source: ListenerTarget): () => void {
    const listeners = LIVE_EVENT_TYPES.map((type) => {
      const listener = () => this.recordEvent(type);
      source.addEventListener(type, listener);
      return [type, listener] as const;
    });
    return () => {
      for (const [type, listener] of listeners) source.removeEventListener(type, listener);
    };
  }

  markOpen() {
    this.lastEventAt = this.now();
  }

  /** True once heartbeats were seen and none of any kind arrived for the limit. */
  isSilent(): boolean {
    return (
      this.sendsHeartbeats &&
      this.now() - this.lastEventAt >= LIVE_STREAM_SILENCE_RESTART_MS
    );
  }

  private recordEvent(type: (typeof LIVE_EVENT_TYPES)[number]) {
    this.lastEventAt = this.now();
    if (type === "heartbeat") this.sendsHeartbeats = true;
  }
}

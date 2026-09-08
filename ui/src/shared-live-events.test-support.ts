// Owns ordered in-memory MessagePort pairs and a controllable upstream SSE.
// Does not emulate browser freezing or networking. Extracted from
// shared-live-events.test.ts for facade and real-App integration coverage.
import { vi } from "vitest";

export class TestPort {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  peer!: TestPort;
  closed = false;
  sent: unknown[] = [];
  start() {}
  close() { this.closed = true; }
  postMessage(data: unknown) {
    if (this.closed) return;
    this.sent.push(data);
    const peer = this.peer;
    queueMicrotask(() => {
      if (!peer.closed) peer.onmessage?.(new MessageEvent("message", { data }));
    });
  }
}

export class TestSource extends EventTarget {
  readyState = 0;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  close = vi.fn(() => { this.readyState = 2; });
  open() { this.readyState = 1; this.onopen?.(); }
  fail(readyState: number) { this.readyState = readyState; this.onerror?.(); }
  emit(type: string, data: string) {
    this.dispatchEvent(new MessageEvent(type, { data }));
  }
}

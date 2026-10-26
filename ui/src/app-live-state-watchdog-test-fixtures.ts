// Existing App watchdog schedules share an authoritative recovery producer.
// Install after initial hydration so cached SSE fixtures cannot answer recovery.
import { vi } from "vitest";
import * as api from "./api";
import type { Session } from "./types";
import { makeSessionHydrationResponse, type AppTestStateResponse } from "./app-test-harness";

export function installWatchdogTailProducer(produce: () => Promise<Response>) {
  const requestedAt: number[] = [];
  const read = vi.spyOn(api, "fetchSessionTail").mockImplementation(async (id, limit = 20) => {
    requestedAt.push(Date.now());
    const state = await (await produce()).json() as AppTestStateResponse;
    const candidate = state.sessions.find(entry => entry.id === id);
    if (!candidate || !("messages" in candidate) || !Array.isArray(candidate.messages)) {
      throw new Error(`Missing recovery session: ${id}`);
    }
    const session = candidate as Session;
    const instance = state.serverInstanceId ?? "test-instance";
    const count = session.messageCount ?? session.messages.length;
    const messages = session.messages.slice(-limit);
    return makeSessionHydrationResponse({ ...session, messages, messageCount: count,
      messageStartIndex: count - messages.length, messagesLoaded: count === messages.length,
      hasOlderHistory: count > messages.length, hasNewerHistory: false,
      bodySeq: state.revision, bodySeqEpoch: instance }, state.revision, instance);
  });
  return Object.assign(read, { requestedAt });
}

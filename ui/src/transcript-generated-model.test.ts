// Deterministic delivery model: immutable reference captures are independent
// of production reconciliation, placement, tail admission and demand.
// Environment tasks cover one local mounted session. Every tail read comes from
// the production gate at task end, retry delivery or a backstop opportunity;
// history pages use master's independent loader.
// Generic rejected reads model catch/finally + capped retry in startSessionHydration.
// Unmount, 404/409 recovery, first unloaded hydration and prefetch are not modelled.
import { describe, expect, it } from "vitest";
import { TranscriptRepairAuthority } from "./transcript-repair-authority";
import { bodySequenceProof, type BodyDelta } from "./transcript-body-sequence";
import { applyDeltaToSessions, applyMetadataOnlySessionDelta } from "./live-updates";
import { decideDeltaRevisionAction } from "./state-revision";
import { decideHttpSessionAdoption, applyHttpSessionEffects } from "./transcript-http-adoption";
import { prependSessionHistoryPage, replaceSessionWithHistoryAroundPage } from "./session-history";
import { releaseSessionHydrationFlight, shouldRequestSessionTailRead,
  SESSION_HYDRATION_MAX_RETRY_ATTEMPTS } from "./app-live-state-hydration";
import type { SessionHydrationRequestContext } from "./session-hydration-adoption";
import type { Session, StateSessionSummary, TextMessage } from "./types";

const INSTANCE = "local-instance";
const text = (id: string, value: string): TextMessage => ({
  id, type: "text", author: "assistant", timestamp: "now", text: value,
});
class Reference {
  seq = 0;
  stamp = 1;
  revision = 10;
  status: Session["status"] = "active";
  bodies = [text("head", "Older"), text("middle", "Middle"), text("last", "Cut")];
  versions = new Map<number, TextMessage[]>();
  touched = new Map<number, string[]>();
  constructor() { this.save([]); }
  save(touched: string[]) {
    this.versions.set(this.seq, structuredClone(this.bodies));
    this.touched.set(this.seq, touched);
  }
  read(start = 0): Session {
    return { id: "a", name: "A", emoji: "AI", agent: "Codex", model: "default",
      workdir: "C:/repo", status: this.status, preview: `Preview ${this.stamp}`,
      sessionMutationStamp: this.stamp, queuePaused: this.status === "idle",
      bodySeqEpoch: INSTANCE, bodySeq: this.seq, messageCount: this.bodies.length,
      messageStartIndex: start, messagesLoaded: start === 0, hasOlderHistory: start > 0,
      hasNewerHistory: false, messages: structuredClone(this.bodies.slice(start)) };
  }
  summary(): StateSessionSummary {
    const { messages: _body, messagesLoaded: _loaded, ...summary } = this.read();
    return structuredClone({ ...summary, messageCount: this.bodies.length, queuePaused: this.status === "idle" });
  }
  update(kind: "append" | "insertion" | "textDelta" | "textReplace" | "messageUpdated", targetIndex?: number): BodyDelta {
    this.seq++; this.stamp++; this.revision++;
    const base = { sessionId: "a", sessionSeq: this.seq, bodySeqEpoch: INSTANCE,
      revision: this.revision, sessionMutationStamp: this.stamp };
    if (kind === "append" || kind === "insertion") {
      const index = kind === "append" ? this.bodies.length : 1;
      const created = text(`new-${this.seq}`, `Created ${this.seq}`);
      this.bodies.splice(index, 0, created); this.save([created.id]);
      return { ...base, type: "messageCreated", messageId: created.id, messageIndex: index,
        messageCount: this.bodies.length, message: structuredClone(created), status: this.status,
        preview: `Preview ${this.stamp}` };
    }
    const index = targetIndex ?? this.bodies.length - 1;
    const before = this.bodies[index];
    const after = { ...before, text: kind === "textDelta" ? before.text + "!" : `Complete ${this.seq}` };
    this.bodies[index] = after; this.save([after.id]);
    const common = { ...base, messageId: after.id, messageIndex: index, messageCount: this.bodies.length };
    return kind === "textDelta" ? { ...common, type: "textDelta", textStartByte: before.text.length, delta: "!" }
      : kind === "textReplace" ? { ...common, type: "textReplace", text: after.text }
      : { ...common, type: "messageUpdated", message: structuredClone(after), status: this.status,
          preview: `Preview ${this.stamp}` };
  }
  metadata() { this.stamp++; this.revision++; this.status = "idle"; }
}

class Client {
  owner: TranscriptRepairAuthority;
  refs = { latestRevision: { current: 10 as number | null },
    instance: { current: INSTANCE as string | null }, seenInstances: { current: new Set([INSTANCE]) } };
  visible = true;
  flight: { answer: Session; revision: number; context: SessionHydrationRequestContext } | null = null;
  readBaseline = 0;
  acceptedMetadataStamp: number | null = 1;
  reads = 0;
  activeHydrations = new Set<string>();
  retryPending = false;
  retryAttempts = 0;
  watchdogFirings = 0;
  suppressFlightEndRequest = false;
  readCauses: string[] = [];
  constructor(reference: Reference, start = 0) {
    this.owner = new TranscriptRepairAuthority(undefined, [reference.read(start)]);
    this.owner.setServerInstance(INSTANCE);
    this.owner.adoptCreatedSession(reference.read(start));
  }
  delta(delta: BodyDelta) {
    const previous = this.owner.sessionsRef.current[0];
    const body = this.owner.receiveBodyDelta(delta);
    const action = decideDeltaRevisionAction(this.refs.latestRevision.current, delta.revision);
    if (body === "legacy" && action === "apply") {
      const result = applyDeltaToSessions(this.owner.sessionsRef.current, delta);
      if (result.kind !== "needsResync") this.owner.commit(result.sessions, "delta");
    } else if (action === "apply" || (action === "ignore" && delta.revision === this.refs.latestRevision.current)) {
      const metadata = applyMetadataOnlySessionDelta(previous, delta);
      this.owner.commit([{ ...metadata, messages: this.owner.sessionsRef.current[0].messages,
        messageCount: this.owner.sessionsRef.current[0].messageCount }], "metadata");
    }
    if (action === "apply") this.refs.latestRevision.current = delta.revision;
  }
  summary(summary: StateSessionSummary, revision: number) {
    if (revision >= (this.refs.latestRevision.current ?? -1)) {
      this.refs.latestRevision.current = revision;
      this.owner.adoptSummaries([summary]);
    }
  }
  task(reference: Reference, cause: string, events: () => void = () => {}) {
    this.suppressFlightEndRequest = false;
    events();
    this.invariant(reference);
    if (!this.suppressFlightEndRequest) this.taskEnd(reference, cause);
  }
  taskEnd(reference: Reference, cause: string) {
    const demand = () => this.owner.needsTailRead("a", this.visible);
    if (!shouldRequestSessionTailRead({ mounted: true,
      inFlight: this.activeHydrations.has("a"), retryPending: this.retryPending,
      needsTailRead: demand })) return;
    expect(demand(), "every autonomous request has demand at issuance").toBe(true);
    expect(this.flight).toBeNull();
    this.activeHydrations.add("a");
    const current = this.owner.sessionsRef.current[0];
    this.flight = { answer: reference.read(1), revision: reference.revision, context: {
      kind: "partialTail", revision: this.refs.latestRevision.current, serverInstanceId: INSTANCE,
      sessionMutationStamp: current.sessionMutationStamp ?? null, messageCount: current.messageCount ?? null } };
    this.reads++;
    this.readCauses.push(cause);
  }
  retryTimer() {
    expect(this.retryPending, "only a scheduled retry timer may fire").toBe(true);
    this.retryPending = false;
  }
  watchdogTick(reference: Reference) {
    this.watchdogFirings++;
    this.invariant(reference);
    if (this.watchdogFirings % 3 === 0) this.taskEnd(reference, "backstop");
  }
  scheduleRejectedReadRetry() {
    // Catch/stale endings suppress immediate handoff even after the fast cap.
    // A later periodic opportunity, not a manufactured answer, reopens demand.
    this.suppressFlightEndRequest = true;
    if (this.owner.needsTailRead("a", this.visible) &&
      this.retryAttempts < SESSION_HYDRATION_MAX_RETRY_ATTEMPTS) {
      this.retryAttempts++;
      this.retryPending = true;
    } else this.retryPending = false;
  }
  navigateAttached(captured: Session, revision: number) {
    const outcome = replaceSessionWithHistoryAroundPage({ current: this.owner.sessionsRef.current[0],
      requestedPosition: 2, page: { messages: captured.messages, messageStartIndex: 1,
        messageCount: 3, sessionMutationStamp: captured.sessionMutationStamp!,
        hasMore: true, hasNewer: false, nextBefore: captured.messages[0].id, nextAfter: null,
        revision, serverInstanceId: INSTANCE } });
    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") this.owner.commit([outcome.session], "history");
  }
  finish(fail = false) {
    const flight = this.flight;
    if (!flight) return false;
    try {
      if (fail) {
        this.scheduleRejectedReadRetry();
        return false;
      }
      const decision = decideHttpSessionAdoption({
        responseSession: flight.answer, responseRevision: flight.revision,
        responseServerInstanceId: INSTANCE, requestContext: flight.context,
        currentSession: this.owner.sessionsRef.current[0], currentRevision: this.refs.latestRevision.current,
        currentServerInstanceId: INSTANCE, seenServerInstanceIds: this.refs.seenInstances.current,
        pairedLocalTail: this.owner.isEligibleLocalRead(flight.answer),
      });
      const revision = this.refs.latestRevision.current;
      if (decision.admission === "none") {
        // The local-domain stale answer uses startSessionHydration's stale
        // outcome retry path, not an immediate second request.
        expect(decision.outcome).toBe("stale");
        this.scheduleRejectedReadRetry();
        return false;
      }
      const adopted = this.owner.adoptTail(flight.answer);
      if (!adopted) {
        this.scheduleRejectedReadRetry();
        return false;
      }
      this.retryAttempts = 0;
      this.retryPending = false;
      this.readBaseline = bodySequenceProof(flight.answer)!.seq;
      applyHttpSessionEffects(decision, this.refs);
      expect(this.refs.latestRevision.current, "tail must not skip queued global frames").toBe(revision);
      return true;
    } finally {
      // The single hydration slot is released on every terminal path.
      this.flight = null;
      releaseSessionHydrationFlight(this.activeHydrations, "a");
    }
  }
  invariant(reference: Reference) {
    const session = this.owner.sessionsRef.current[0];
    const stamp = session.sessionMutationStamp;
    if (typeof stamp === "number" && this.acceptedMetadataStamp !== null) {
      expect(stamp, "same-instance known numeric metadata is monotone")
        .toBeGreaterThanOrEqual(this.acceptedMetadataStamp);
    }
    this.acceptedMetadataStamp = typeof stamp === "number" ? stamp : null;
    const certificate = this.owner.bodyCertificate("a");
    if (!certificate || session.hasNewerHistory) return;
    const bodies = reference.versions.get(certificate.appliedSeq)!;
    const touched = new Set([...reference.touched].filter(([seq]) => seq > this.readBaseline &&
      seq <= certificate.appliedSeq).flatMap(([, ids]) => ids));
    const start = session.messageStartIndex ?? 0;
    session.messages.forEach((message, index) => {
      if (touched.has(message.id)) expect(message, "touched stream body and position match reference")
        .toEqual(bodies[start + index]);
    });
  }
  quiesce(reference: Reference) {
    this.task(reference, "final summary", () => this.summary(reference.summary(), reference.revision));
    for (let attempt = 0; attempt < 4 && (this.flight || this.retryPending ||
      this.owner.needsTailRead("a", this.visible)); attempt++) {
      if (this.retryPending) this.task(reference, "retry timer", () => this.retryTimer());
      if (!this.flight && !this.owner.needsTailRead("a", this.visible)) break;
      // These finite schedules assume continuing ticks, settled blockers and
      // a subsequent admissible response. No bound is claimed for a browser
      // that suspends timers or a server that only returns stale answers.
      if (!this.flight) for (let tick = 0; tick < 3; tick++) this.watchdogTick(reference);
      expect(this.flight, "standing demand must reach the periodic shared gate").not.toBeNull();
      this.task(reference, "answer", () => { this.finish(); });
    }
    expect(this.flight).toBeNull();
    expect(this.activeHydrations.size).toBe(0);
    expect(this.owner.needsTailRead("a", true)).toBe(false);
    const messages = this.owner.sessionsRef.current[0].messages;
    expect(messages[messages.length - 1]).toEqual(reference.bodies[reference.bodies.length - 1]);
  }
}

function permutations<T>(values: T[]): T[][] {
  if (!values.length) return [[]];
  return values.flatMap((value, index) => permutations(values.filter((_, i) => i !== index))
    .map(rest => [value, ...rest]));
}

describe("generated local attached-tail admission", () => {
  it("serves demand after fast retry exhaustion without another edge notification", () => {
    const server = new Reference(), client = new Client(server);
    server.update("textReplace");
    client.task(server, "summary ahead", () => client.summary(server.summary(), server.revision));
    for (let attempt = 0; attempt <= SESSION_HYDRATION_MAX_RETRY_ATTEMPTS; attempt++) {
      expect(client.flight).not.toBeNull();
      client.task(server, "rejected answer", () => { client.finish(true); });
      if (attempt < SESSION_HYDRATION_MAX_RETRY_ATTEMPTS) {
        expect(client.retryPending).toBe(true);
        client.task(server, "retry timer", () => client.retryTimer());
      }
    }
    expect(client.retryPending).toBe(false);
    expect(client.flight).toBeNull();
    const reads = client.reads;
    client.watchdogTick(server);
    client.watchdogTick(server);
    expect(client.reads).toBe(reads);
    client.watchdogTick(server);
    expect(client.reads).toBe(reads + 1);
    expect(client.readCauses[client.readCauses.length - 1]).toBe("backstop");
    client.task(server, "admissible answer", () => { expect(client.finish()).toBe(true); });
    expect(client.owner.needsTailRead("a", true)).toBe(false);
    const messages = client.owner.sessionsRef.current[0].messages;
    expect(messages[messages.length - 1]).toEqual(server.bodies[server.bodies.length - 1]);
  });
  it("enumerates attached replacing navigation against a stale-global next body frame", () => {
    for (const order of permutations(["delta", "navigation"] as const)) {
      const server = new Reference(), client = new Client(server);
      const captured = server.read(1), capturedRevision = server.revision;
      const delta = server.update("textReplace");
      // Body placement is independent of suppressed metadata, leaving the
      // captured page's stamp comparable while its body is behind the stream.
      const staleGlobal = { ...delta, revision: 9, sessionMutationStamp: 1 };
      for (const event of order) {
        client.task(server, event, () => {
          if (event === "delta") client.delta(staleGlobal);
          else client.navigateAttached(captured, capturedRevision);
        });
      }
      expect(client.owner.bodyCertificate("a")).toBeNull();
      expect(client.owner.needsTailRead("a", true)).toBe(true);
      // Settle from navigation demand alone: no final summary or rescue read.
      expect(client.readCauses).toEqual(["navigation"]);
      client.task(server, "answer", () => { expect(client.finish()).toBe(true); });
      expect(client.reads).toBe(1);
      expect(client.owner.needsTailRead("a", true)).toBe(false);
      const resident = client.owner.sessionsRef.current[0].messages;
      expect(resident[resident.length - 1]).toEqual(server.bodies[server.bodies.length - 1]);
    }
  });

  it.each(["append", "insertion", "textDelta", "textReplace", "messageUpdated"] as const)(
    "enumerates all captured summary/read/frame orders for %s", operation => {
      for (const cause of ["lagged", "summary ahead"] as const) {
        for (const dropFinal of [false, true]) {
          const events = ["oldSummary", "answer", "first", "last", "newSummary"] as const;
          const orders = cause === "lagged" ? permutations([...events])
            : permutations(events.filter(event => event !== "oldSummary"));
          for (const order of orders
            .filter(order => dropFinal || order.indexOf("first") < order.indexOf("last"))) {
            const server = new Reference();
            const client = new Client(server);
            let first!: BodyDelta;
            client.task(server, "server first body", () => { first = server.update(operation); });
            const oldSummary = server.summary(), oldRevision = server.revision;
            client.task(server, cause, () => {
              if (cause === "lagged") client.owner.declareLoss("lagged");
              else client.summary(oldSummary, oldRevision);
            });
            expect(client.readCauses).toEqual([cause]);
            let last!: BodyDelta;
            client.task(server, "server final body", () => { last = server.update("textDelta"); });
            client.task(server, "server metadata", () => server.metadata());
            const newSummary = server.summary(), newRevision = server.revision;
            for (const event of order) {
              client.task(server, event, () => {
                if (event === "oldSummary") client.summary(oldSummary, oldRevision);
                if (event === "newSummary") client.summary(newSummary, newRevision);
                if (event === "answer") client.finish();
                if (event === "first") client.delta(first);
                if (event === "last" && !dropFinal) client.delta(last);
              });
            }
            client.quiesce(server);
            expect(client.reads).toBeLessThanOrEqual(4);
          }
        }
      }
    });

  it("rejects a captured old summary after newer HTTP metadata without skipping its global revision", () => {
    const server = new Reference(), client = new Client(server);
    const captured = server.summary(), revision = server.revision;
    client.task(server, "server body", () => { server.update("textReplace"); });
    client.task(server, "server metadata", () => server.metadata());
    client.task(server, "lagged", () => client.owner.declareLoss("lagged"));
    client.task(server, "answer", () => { expect(client.finish()).toBe(true); });
    const stamp = client.owner.sessionsRef.current[0].sessionMutationStamp;
    client.task(server, "captured summary", () => client.summary(captured, revision));
    expect(client.owner.sessionsRef.current[0].sessionMutationStamp).toBe(stamp);
    expect(client.owner.sessionsRef.current[0].status).toBe("idle");
    expect(client.refs.latestRevision.current).toBe(revision);
  });

  it("a rejected read releases the sole flight but waits for its retry timer", () => {
    const server = new Reference(), client = new Client(server);
    client.task(server, "server body", () => { server.update("textReplace"); });
    client.task(server, "summary ahead", () => client.summary(server.summary(), server.revision));
    expect(client.reads).toBe(1);
    client.task(server, "rejected answer", () => { client.finish(true); });
    expect(client.activeHydrations.size).toBe(0);
    expect(client.owner.needsTailRead("a", true)).toBe(true);
    client.task(server, "summary while retry pending", () => client.summary(server.summary(), server.revision));
    expect(client.flight).toBeNull();
    expect(client.reads).toBe(1);
    client.task(server, "retry timer", () => client.retryTimer());
    expect(client.readCauses).toEqual(["summary ahead", "retry timer"]);
    client.quiesce(server);
  });

  it("same-task summary then own next delta closes demand without a read", () => {
    const server = new Reference(), client = new Client(server);
    const next = server.update("textReplace");
    client.task(server, "same task summary and own delta", () => {
      client.summary(server.summary(), server.revision);
      expect(client.owner.needsTailRead("a", true)).toBe(true);
      client.delta(next);
    });
    expect(client.owner.needsTailRead("a", true)).toBe(false);
    expect(client.reads).toBe(0);
  });

  it("does not schedule a retry when demand closes before rejected completion", () => {
    const server = new Reference(), client = new Client(server);
    const next = server.update("textReplace");
    client.task(server, "summary ahead", () => client.summary(server.summary(), server.revision));
    expect(client.reads).toBe(1);
    client.task(server, "own delta", () => client.delta(next));
    expect(client.owner.needsTailRead("a", true)).toBe(false);
    client.task(server, "rejected answer", () => client.finish(true));
    expect(client.retryPending, "closed demand schedules no back-off timer").toBe(false);
    expect(client.activeHydrations.size).toBe(0);
    expect(client.reads).toBe(1);
    client.quiesce(server);
  });

  it("separate summary and own delta tasks initiate exactly one read", () => {
    const server = new Reference(), client = new Client(server);
    const next = server.update("textReplace");
    client.task(server, "summary ahead", () => client.summary(server.summary(), server.revision));
    client.task(server, "own delta", () => client.delta(next));
    expect(client.readCauses).toEqual(["summary ahead"]);
    client.quiesce(server);
    expect(client.reads).toBe(1);
  });

  it("a missing middle-body update followed by a placeable final replacement is a hole", () => {
    const server = new Reference(), client = new Client(server);
    client.task(server, "lost middle frame", () => { server.update("messageUpdated", 1); });
    const last = server.update("textReplace");
    client.task(server, "hole", () => client.delta(last));
    expect(client.owner.needsTailRead("a", true)).toBe(true);
    expect(client.readCauses).toEqual(["hole"]);
    client.quiesce(server);
    expect(client.owner.sessionsRef.current[0].messages[1]).toEqual(server.bodies[1]);
  });

  it("ordinary older paging never promotes the retained certificate; detaching drops it", () => {
    const server = new Reference(), client = new Client(server, 1);
    server.update("textReplace");
    const read = server.read();
    const current = client.owner.sessionsRef.current[0];
    const outcome = prependSessionHistoryPage({ current, requestedBefore: current.messages[0].id,
      page: { messages: read.messages.slice(0, 1), messageStartIndex: 0, messageCount: 3,
        sessionMutationStamp: current.sessionMutationStamp!, hasMore: false, hasNewer: true,
        nextBefore: null, nextAfter: read.messages[0].id,
        revision: server.revision, serverInstanceId: INSTANCE } });
    expect(outcome.kind).toBe("applied");
    client.task(server, "older page", () => {
      if (outcome.kind === "applied") client.owner.commit([outcome.session], "history");
    });
    expect(client.owner.bodyCertificate("a")?.appliedSeq).toBe(0);
    client.task(server, "detach", () => client.owner.commit([
      { ...client.owner.sessionsRef.current[0], hasNewerHistory: true }], "history"));
    expect(client.owner.bodyCertificate("a")).toBeNull();
    expect(client.owner.needsTailRead("a", true)).toBe(false);
    // app-live-state's explicit history demand invokes the master page loader,
    // independently of the tail hydration slot. Its replacement landing drops
    // proof; only the task-end requester may initiate the subsequent tail read.
    const captured = server.read(1), revision = server.revision;
    client.task(server, "attached page landing", () => client.navigateAttached(captured, revision));
    expect(client.readCauses).toEqual(["attached page landing"]);
    client.task(server, "answer", () => { expect(client.finish()).toBe(true); });
    client.quiesce(server);
  });

  it("unchanged paired snapshots preserve list and Session identity, including hidden uncertified sessions", () => {
    const server = new Reference();
    const owner = new TranscriptRepairAuthority(undefined, [server.read()]);
    owner.setServerInstance(INSTANCE);
    owner.adoptSummaries([server.summary()]);
    const list = owner.sessionsRef.current, session = list[0];
    owner.adoptSummaries([server.summary()]);
    expect(owner.sessionsRef.current).toBe(list);
    expect(owner.sessionsRef.current[0]).toBe(session);
    expect(owner.needsTailRead("a", false)).toBe(false);
    expect(owner.needsTailRead("a", true)).toBe(true);
  });

  it("a loaded observed pair without a certificate requests only on display", () => {
    const server = new Reference(), client = new Client(server);
    client.owner = new TranscriptRepairAuthority(undefined, [server.read()]);
    client.owner.setServerInstance(INSTANCE);
    client.visible = false;
    client.task(server, "hidden summary", () => client.summary(server.summary(), server.revision));
    expect(client.reads).toBe(0);
    client.task(server, "display uncertified loaded pair", () => { client.visible = true; });
    expect(client.readCauses).toEqual(["display uncertified loaded pair"]);
    client.quiesce(server);
  });

  it.each([undefined, null])("admits an incomparable unknown stamp %s after a paired read", stamp => {
    const server = new Reference(), client = new Client(server);
    const newer = { ...server.summary(), status: "idle" as const,
      preview: "New unstamped snapshot", sessionMutationStamp: stamp };
    client.task(server, "unknown stamp summary", () => client.summary(newer, server.revision + 1));
    expect(client.owner.sessionsRef.current[0]).toMatchObject({ status: "idle",
      preview: "New unstamped snapshot", sessionMutationStamp: stamp });
    expect(client.refs.latestRevision.current).toBe(server.revision + 1);
    expect(client.owner.bodyCertificate("a")?.appliedSeq).toBe(server.seq);
  });

  it("fixed-seed long schedules converge with bounded demand-only reads and visibility changes", () => {
    for (const seed of [1, 7, 19, 41, 83, 127, 251, 509]) {
      let state = seed;
      const choose = (n: number) => { state = (Math.imul(state, 1664525) + 1013904223) >>> 0; return state % n; };
      const server = new Reference(), client = new Client(server);
      let rejected = false;
      const kinds = ["append", "insertion", "textDelta", "textReplace", "messageUpdated"] as const;
      for (let step = 0; step < 24; step++) {
        let delta!: BodyDelta;
        client.task(server, "server body", () => { delta = server.update(kinds[choose(kinds.length)]); });
        const summary = server.summary(), revision = server.revision;
        const deliveries = () => {
          if (choose(4) !== 0) client.delta(delta);
          if (choose(2)) client.summary(summary, revision);
        };
        if (choose(2)) client.task(server, "grouped delivery", deliveries);
        else {
          client.task(server, "delta delivery", () => { if (choose(4)) client.delta(delta); });
          client.task(server, "summary delivery", () => { if (choose(2)) client.summary(summary, revision); });
        }
        client.task(server, "visibility", () => { client.visible = choose(4) !== 0; });
        if (client.retryPending && choose(2)) client.task(server, "retry timer", () => client.retryTimer());
        if (client.flight && choose(2)) client.task(server, "answer", () => {
          const fail = !rejected && choose(5) === 0;
          rejected ||= fail;
          client.finish(fail);
        });
      }
      client.task(server, "visible", () => { client.visible = true; });
      client.quiesce(server);
      // 24 server steps, four settlement reads and the capped retry allowance.
      expect(client.reads).toBeLessThanOrEqual(24 + 4 + SESSION_HYDRATION_MAX_RETRY_ATTEMPTS);
    }
  });

  it("the shared four-condition gate keeps blocked demand lazy", () => {
    for (const mounted of [false, true]) for (const inFlight of [false, true]) {
      for (const retryPending of [false, true]) for (const demand of [false, true]) {
        let calls = 0;
        const allowed = mounted && !inFlight && !retryPending;
        expect(shouldRequestSessionTailRead({ mounted, inFlight, retryPending,
          needsTailRead: () => { calls++; return demand; } })).toBe(allowed && demand);
        expect(calls).toBe(allowed ? 1 : 0);
      }
    }
  });
});

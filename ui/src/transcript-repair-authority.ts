// Owns the local attached-tail certificate and the single publication gate.
// Older pages and remote/unpaired sessions keep the ordinary history policy.
// The hook owns the sole hydration flight; this owner has no read lifecycle.
import type { Session, StateSessionSummary } from "./types";
import type { DraftImageAttachment } from "./app-utils";
import { syncComposerSessionsStoreIncremental } from "./session-store";
import { bodySequenceProof, MAX_KEPT_BODY_DELTAS, type BodyDelta } from "./transcript-body-sequence";
import { applyBodyDeltaToRange } from "./transcript-body-placement";
import { decodeTargetedSessionProjection } from "./targeted-session-projection";
import { reconcileSingleSession, reconcileStateSessionSummaries } from "./session-reconcile";
import { mergeAppendOnlyPartialTailCoverage, type SessionHydrationRequestContext } from "./session-hydration-adoption";

export type SessionReadRef = { readonly current: readonly Session[] };
export type SessionCommitProvenance = "metadata" | "delta" | "history";
type TailState = {
  kind: "uncertified" | "certified" | "dirty";
  admittedBodySnapshot: boolean;
  appliedSeq?: number;
  observedSeq: number;
  kept: Map<number, BodyDelta>;
};
type PublicationSinks = {
  publish: (next: () => Session[]) => void;
  drafts: () => Record<string, string>;
  attachments: () => Record<string, DraftImageAttachment[]>;
};

const bodyFields = (session: Session) => ({
  messages: session.messages, messageCount: session.messageCount,
  messagesLoaded: session.messagesLoaded, messageStartIndex: session.messageStartIndex,
  hasOlderHistory: session.hasOlderHistory, hasNewerHistory: session.hasNewerHistory,
});
const identical = (a: Session, b: Session) => {
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  return [...keys].every(key => a[key as keyof Session] === b[key as keyof Session]);
};

// Ordinary history can extend beside the resident window without replacing
// its bodies. Every other body-window change needs a new eligible tail read;
// the page's wire proof cannot certify that replacement.
function retainsResidentHistoryWindow(prior: Session, next: Session) {
  const start = (session: Session) => session.messagesLoaded === true ? 0
    : session.messageStartIndex ?? Math.max(0, (session.messageCount ?? session.messages.length) - session.messages.length);
  const offset = start(prior) - start(next);
  if (offset < 0 || offset + prior.messages.length > next.messages.length) return false;
  if (prior.messages === next.messages) return offset === 0;
  return prior.messages.length > 0 && next.messages.length > prior.messages.length &&
    prior.messages.every((message, index) => next.messages[offset + index] === message);
}

// A paired tail is authoritative at and after its starting position. An
// aligned id boundary permits retaining the head, but does not certify that
// historical prefix's spatial currency. Legacy history helpers stay unchanged.
function replaceLocalTail(current: Session | undefined, read: Session): Session | null {
  const count = read.messageCount ?? read.messages.length;
  const start = read.messageStartIndex ?? Math.max(0, count - read.messages.length);
  if (!Number.isSafeInteger(count) || !Number.isSafeInteger(start) ||
      start < 0 || start + read.messages.length !== count) return null;
  if (count === 0) return { ...read, messages: [], messageStartIndex: 0,
    messagesLoaded: true, hasOlderHistory: false, hasNewerHistory: false };
  if (!read.messages.length) return null;
  const currentStart = current?.messageStartIndex ?? Math.max(0,
    (current?.messageCount ?? current?.messages.length ?? 0) - (current?.messages.length ?? 0));
  const boundary = start - currentStart;
  const incomingIds = new Set(read.messages.map(message => message.id));
  const prefix = current && current.hasNewerHistory !== true && boundary >= 0 &&
    boundary < current.messages.length && current.messages[boundary]?.id === read.messages[0].id &&
    currentStart + current.messages.length <= count
    ? current.messages.slice(0, boundary) : [];
  const aligned = prefix.every(message => !incomingIds.has(message.id));
  const retained = aligned ? prefix : [];
  const messageStartIndex = retained.length ? currentStart : start;
  return { ...read, messages: retained.length ? [...retained, ...read.messages] : read.messages,
    messageStartIndex, messagesLoaded: messageStartIndex === 0,
    hasOlderHistory: messageStartIndex > 0, hasNewerHistory: false };
}

export class TranscriptRepairAuthority {
  private sessions: Session[];
  private serverInstanceId: string | null = null;
  private resetMetadataDomain = false;
  private tails = new Map<string, TailState>();
  readonly sessionsRef: SessionReadRef;

  constructor(private readonly sinks?: PublicationSinks, initial: Session[] = []) {
    this.sessions = initial;
    const owner = this;
    this.sessionsRef = { get current() { return owner.sessions; } };
  }

  setServerInstance(instance: string | null) {
    if (!instance || instance === this.serverInstanceId) return;
    this.resetMetadataDomain = this.serverInstanceId !== null;
    this.serverInstanceId = instance;
    this.tails.clear();
  }

  isEligibleLocalRead(session: Session, instance = this.serverInstanceId) {
    const proof = bodySequenceProof(session);
    return !!instance && instance === this.serverInstanceId && !session.remoteId &&
      proof?.epoch === instance;
  }

  private state(id: string): TailState {
    let state = this.tails.get(id);
    if (!state) {
      state = { kind: "uncertified", admittedBodySnapshot: false, observedSeq: -1, kept: new Map() };
      this.tails.set(id, state);
    }
    return state;
  }

  private observe(session: Session | StateSessionSummary) {
    if (!this.isEligibleLocalRead(session as Session)) return;
    const proof = bodySequenceProof(session)!;
    const state = this.state(session.id);
    state.observedSeq = Math.max(state.observedSeq, proof.seq);
  }

  // Pure demand. The hook invokes it at task end, so a summary followed by
  // its own next delta in that task closes suspicion without another read.
  needsTailRead(id: string, visible: boolean) {
    const session = this.sessions.find(entry => entry.id === id);
    const state = this.tails.get(id);
    return visible && !!session && !session.remoteId && session.hasNewerHistory !== true &&
      !!state && (state.kind === "dirty" ||
        (state.observedSeq >= 0 && (state.kind === "certified"
          ? state.observedSeq > state.appliedSeq! : session.messagesLoaded === true || state.admittedBodySnapshot)));
  }

  bodyCertificate(id: string) {
    const state = this.tails.get(id);
    return state?.kind === "certified" && this.serverInstanceId
      ? { epoch: this.serverInstanceId, appliedSeq: state.appliedSeq! } : null;
  }

  declareLoss(cause: "lagged" | "serverInstanceChanged") {
    if (cause === "serverInstanceChanged") {
      this.tails.clear();
      this.resetMetadataDomain = true;
    } else {
      for (const state of this.tails.values()) {
        if (state.kind === "certified") state.kind = "dirty";
      }
    }
    // Dirty is demand, never a reason to remove displayed bodies.
    return this.sessions;
  }

  adoptSummaries(summaries: StateSessionSummary[],
    options?: Parameters<typeof reconcileStateSessionSummaries>[2]) {
    const metadata = summaries.map(summary => {
      this.observe(summary);
      const { bodySeq: _seq, bodySeqEpoch: _epoch, ...rest } = summary;
      return rest;
    });
    return this.commit(reconcileStateSessionSummaries(this.sessions, metadata, options), "metadata");
  }

  receiveBodyDelta(delta: BodyDelta & { serverInstanceId?: string }): "legacy" | "late" | "applied" | "repair" {
    const resident = this.sessions.find(session => session.id === delta.sessionId);
    const proof = bodySequenceProof(delta);
    if (!resident || resident.remoteId || !this.serverInstanceId ||
        (delta.serverInstanceId !== undefined && delta.serverInstanceId !== this.serverInstanceId) ||
        proof?.epoch !== this.serverInstanceId) return "legacy";
    const state = this.state(delta.sessionId);
    state.observedSeq = Math.max(state.observedSeq, proof.seq);
    if (state.appliedSeq !== undefined && proof.seq <= state.appliedSeq) return "late";
    state.kept.set(proof.seq, delta);
    if (state.kept.size > MAX_KEPT_BODY_DELTAS) state.kept.delete(state.kept.keys().next().value!);
    if (resident.hasNewerHistory === true) {
      state.kind = "uncertified";
      state.appliedSeq = undefined;
      return "legacy";
    }
    if (state.kind === "uncertified") return "legacy";
    if (state.kind === "dirty" || proof.seq !== state.appliedSeq! + 1) {
      state.kind = "dirty";
      return "repair";
    }
    const placed = applyBodyDeltaToRange(resident, delta);
    if (!placed) {
      state.kind = "dirty";
      return "repair";
    }
    state.appliedSeq = proof.seq;
    // Global revision admission owns metadata independently of this placement.
    this.commit(this.sessions.map(session => session.id === delta.sessionId
      ? { ...resident, ...bodyFields(placed) } : session), "delta");
    return "applied";
  }

  private replayTail(raw: Session): Session | null {
    const proof = bodySequenceProof(raw)!;
    const state = this.state(raw.id);
    const newest = Math.max(proof.seq, state.observedSeq, state.appliedSeq ?? -1);
    let read = replaceLocalTail(this.sessions.find(session => session.id === raw.id), raw);
    if (!read) return null;
    for (let seq = proof.seq + 1; seq <= newest; seq++) {
      const delta = state.kept.get(seq);
      if (!delta) return null;
      read = applyBodyDeltaToRange(read, delta);
      if (!read) return null;
    }
    // Only these two paths write proof: admitted body snapshot and next delta.
    state.kind = "certified";
    state.admittedBodySnapshot = true;
    state.appliedSeq = newest;
    state.observedSeq = newest;
    return read;
  }

  adoptTail(raw: Session, options?: {
    outcome: "adopted" | "partial" | "partialCoverage";
    requestContext: SessionHydrationRequestContext;
  }): Session[] | null {
    const current = this.sessions.find(session => session.id === raw.id);
    if (!current) return null;
    const targeted = decodeTargetedSessionProjection(raw, current);
    if (this.isEligibleLocalRead(raw) && current.hasNewerHistory !== true) {
      const read = this.replayTail(targeted);
      if (!read) return null;
      return this.commit(this.sessions.map(session => session.id === raw.id ? read : session), "delta");
    }
    if (!options) return null;
    // Excluded sessions retain master's adoption, including detached windows.
    const preserve = current.hasNewerHistory === true && current.messages.length > 0;
    const incoming = new Map(targeted.messages.map(message => [message.id, message]));
    const hydrated = { ...targeted, messages: preserve
      ? current.messages.map(message => incoming.get(message.id) ?? message) : targeted.messages,
      messagesLoaded: !preserve && options.outcome === "adopted",
      messageStartIndex: preserve ? current.messageStartIndex : targeted.messageStartIndex,
      hasOlderHistory: preserve ? current.hasOlderHistory : options.outcome === "partial",
      hasNewerHistory: preserve };
    const coverage = options.outcome === "partialCoverage"
      ? mergeAppendOnlyPartialTailCoverage(targeted, current, options.requestContext) : null;
    if (options.outcome === "partialCoverage" && !coverage) return null;
    const session = coverage ?? reconcileSingleSession(current, hydrated, {
      adoptPartialMessages: preserve || options.outcome === "partial", disableMutationStampFastPath: true,
    });
    return this.commit(this.sessions.map(entry => entry.id === raw.id ? session : entry), "delta");
  }

  adoptCreatedSession(raw: Session) {
    let session = decodeTargetedSessionProjection(raw, this.sessions.find(entry => entry.id === raw.id));
    if (this.isEligibleLocalRead(raw) && raw.hasNewerHistory !== true) {
      const read = this.replayTail(session);
      if (!read) return null;
      session = read;
    }
    return this.commit(this.sessions.some(entry => entry.id === raw.id)
      ? this.sessions.map(entry => entry.id === raw.id ? session : entry) : [...this.sessions, session], "delta");
  }

  commit(next: readonly Session[], provenance: SessionCommitProvenance): Session[] {
    const previous = this.sessions;
    const previousById = new Map(previous.map(session => [session.id, session]));
    const projected = next.map(incoming => {
      const prior = previousById.get(incoming.id);
      if (incoming === prior) return incoming;
      this.observe(incoming);
      let session = incoming;
      // Paired local body reads can put metadata ahead of the global ledger.
      // Only that domain compares known stamps; unknown stamps and excluded
      // sessions retain ordinary global admission. Body effects stay separate.
      if (prior && !this.resetMetadataDomain &&
          !prior.remoteId && !incoming.remoteId &&
          this.tails.get(incoming.id)?.admittedBodySnapshot === true &&
          typeof incoming.sessionMutationStamp === "number" &&
          typeof prior.sessionMutationStamp === "number" &&
          incoming.sessionMutationStamp < prior.sessionMutationStamp) {
        session = { ...prior, ...bodyFields(incoming) };
      }
      const state = this.tails.get(session.id);
      if (provenance === "metadata" && prior && state?.kind === "certified") {
        session = { ...session, ...bodyFields(prior) };
      }
      if (state && (session.hasNewerHistory === true ||
          (provenance === "history" && prior && !retainsResidentHistoryWindow(prior, session)))) {
        state.kind = "uncertified";
        state.appliedSeq = undefined;
      }
      // Raw wire fields never certify a resident window.
      const certificate = this.bodyCertificate(session.id);
      if (session.bodySeq !== certificate?.appliedSeq || session.bodySeqEpoch !== certificate?.epoch) {
        session = { ...session, bodySeq: certificate?.appliedSeq, bodySeqEpoch: certificate?.epoch };
      }
      return prior && identical(prior, session) ? prior : session;
    });
    this.resetMetadataDomain = false;
    this.sessions = projected.every((session, index) => session === next[index])
      ? next as Session[] : projected.length === previous.length &&
        projected.every((session, index) => session === previous[index]) ? previous : projected;
    const ids = new Set(this.sessions.map(session => session.id));
    for (const id of this.tails.keys()) if (!ids.has(id)) this.tails.delete(id);
    this.sinks && syncComposerSessionsStoreIncremental({
      changedSessions: this.sessions.filter(session => previousById.get(session.id) !== session),
      removedSessionIds: previous.filter(session => !ids.has(session.id)).map(session => session.id),
      draftsBySessionId: this.sinks.drafts(), draftAttachmentsBySessionId: this.sinks.attachments(),
    });
    return this.sessions;
  }

  publish() { this.sinks?.publish(() => this.sessions); }

  syncSlices(sessionIds: Iterable<string>, removedSessionIds: string[] = []) {
    const ids = new Set(sessionIds);
    this.sinks && syncComposerSessionsStoreIncremental({
      changedSessions: this.sessions.filter(session => ids.has(session.id)), removedSessionIds,
      draftsBySessionId: this.sinks.drafts(), draftAttachmentsBySessionId: this.sinks.attachments(),
    });
  }
}

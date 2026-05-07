// Owns the Phase 2/3 delegation command surface that UI/MCP wrappers can bind
// to: spawn, reviewer batches, status, result, cancel, and polling wait helpers.
// Does not own route transport, live SSE adoption, or backend delegation
// lifecycle rules. New module staged ahead of the TermAl MCP wrapper integration.

import {
  cancelDelegation,
  createDelegation,
  fetchDelegationResult,
  fetchDelegationStatus,
  type AbortableRequestOptions,
  type CreateDelegationRequest,
  type DelegationResponse,
  type DelegationResultResponse,
  type DelegationStatusResponse,
} from "./api";
import {
  mismatchedDelegationIdErrorPacket,
  mixedServerInstanceErrorPacket,
  spawnDelegationFailurePacket,
  spawnDelegationValidationFailurePacket,
  statusFetchErrorPacket,
  type MixedServerInstanceRecoveryGroup,
  type SpawnDelegationFailurePacket,
  type SpawnDelegationTransportFailurePacket,
  type SpawnReviewerBatchErrorPacket,
  type WaitDelegationErrorPacket,
} from "./delegation-error-packets";
import type {
  DelegationCommandResult,
  DelegationFinding,
  DelegationRecord,
  DelegationResult,
  DelegationSummary,
  DelegationStatus,
  Session,
} from "./types";

export type {
  MixedServerInstanceRecoveryGroup,
  WaitDelegationErrorKind,
  WaitDelegationErrorPacket,
} from "./delegation-error-packets";

export const DEFAULT_DELEGATION_WAIT_INTERVAL_MS = 1000;
export const DEFAULT_DELEGATION_WAIT_TIMEOUT_MS = 5 * 60 * 1000;
export const MIN_DELEGATION_WAIT_INTERVAL_MS = 500;
export const MAX_DELEGATION_WAIT_TIMEOUT_MS = 30 * 60 * 1000;
export const MAX_DELEGATION_WAIT_IDS = 10;
// Keep in sync with `MAX_DELEGATION_PROMPT_BYTES` in `src/delegations.rs`.
// If this changes, update the parity-pin test in `delegation-commands.test.ts`.
export const MAX_DELEGATION_PROMPT_BYTES = 64 * 1024;
// Child session names are exposed through redacted delegation summaries; cap
// caller-supplied titles as metadata, not prompt-sized payloads. Keep in sync
// with `MAX_DELEGATION_TITLE_CHARS` in `src/delegations.rs`.
export const MAX_DELEGATION_TITLE_CHARS = 200;
// Explicit model names are metadata echoed in summaries and child cards, not
// prompt payloads. Keep in sync with `MAX_DELEGATION_MODEL_CHARS` in
// `src/delegations.rs`.
export const MAX_DELEGATION_MODEL_CHARS = 200;
// Keep in sync with `MAX_RUNNING_DELEGATIONS_PER_PARENT` in
// `src/delegations.rs`. The backend remains authoritative when a parent already
// has active delegations; this only caps one helper call.
export const MAX_REVIEWER_BATCH_SIZE = 4;
const UNSAFE_TRANSPORT_ID_PATTERN = /[/?#\u0000-\u001f\u007f]/u;

// These client-side wait limits compose to at most 20 status requests/sec per
// wait call. They intentionally do not mirror backend delegation fan-out limits;
// the backend remains authoritative and the MCP wrapper should still add a
// process-level concurrency cap before exposing this surface to untrusted
// callers.

/**
 * Route binding for delegation commands.
 *
 * Custom transports own path-segment encoding and identity preservation for all
 * ids they receive. Command entrypoints trim and reject empty ids, path
 * separators, query/fragment markers, and control characters before invoking a
 * transport.
 */
export type DelegationCommandTransport = {
  createDelegation: (
    parentSessionId: string,
    request: CreateDelegationRequest,
  ) => Promise<DelegationResponse>;
  fetchDelegationStatus: (
    parentSessionId: string,
    delegationId: string,
    options?: AbortableRequestOptions,
  ) => Promise<DelegationStatusResponse>;
  fetchDelegationResult: (
    parentSessionId: string,
    delegationId: string,
  ) => Promise<DelegationResultResponse>;
  cancelDelegation: (
    parentSessionId: string,
    delegationId: string,
  ) => Promise<DelegationStatusResponse>;
};

export const browserDelegationCommandTransport: DelegationCommandTransport = {
  createDelegation,
  fetchDelegationStatus,
  fetchDelegationResult,
  cancelDelegation,
};

export type DelegationChildSessionSummary = {
  id: string;
  name: string;
  emoji: string;
  agent: Session["agent"];
  model: string;
  status: Session["status"];
  parentDelegationId: string | null;
};

export type SpawnDelegationCommandSuccessResult = {
  outcome: "completed";
  delegationId: string;
  childSessionId: string;
  delegation: DelegationSummary;
  childSession: DelegationChildSessionSummary;
  revision: number;
  serverInstanceId: string;
  error?: never;
};

export type SpawnDelegationCommandErrorResult = {
  outcome: "error";
  revision: null;
  serverInstanceId: null;
  error: SpawnDelegationFailurePacket;
};

export type SpawnDelegationCommandResult =
  | SpawnDelegationCommandSuccessResult
  | SpawnDelegationCommandErrorResult;

export type SpawnReviewerBatchItem = Omit<
  CreateDelegationRequest,
  "mode" | "writePolicy"
>;

export type SpawnReviewerBatchFailure =
  SpawnDelegationTransportFailurePacket & {
    index: number;
    title: string | null;
  };

export type SpawnReviewerBatchOutcome = "completed" | "partial" | "error";

export type SpawnReviewerBatchBaseResult = {
  spawned: SpawnDelegationCommandSuccessResult[];
  failed: SpawnReviewerBatchFailure[];
  delegationIds: string[];
  childSessionIds: string[];
  revision: number | null;
  serverInstanceId: string | null;
};

export type SpawnReviewerBatchSuccessResult = SpawnReviewerBatchBaseResult & {
  outcome: "completed" | "partial";
  error?: never;
};

export type SpawnReviewerBatchErrorResult = SpawnReviewerBatchBaseResult & {
  outcome: "error";
  error: SpawnReviewerBatchErrorPacket;
};

export type SpawnReviewerBatchCommandResult =
  | SpawnReviewerBatchSuccessResult
  | SpawnReviewerBatchErrorResult;

export type DelegationStatusCommandResult = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  delegation: DelegationSummary;
  revision: number;
  serverInstanceId: string;
};

export type DelegationResultPacket = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
  findings: DelegationFinding[];
  changedFiles: string[];
  commandsRun: DelegationCommandResult[];
  notes: string[];
  revision: number;
  serverInstanceId: string;
};

export type WaitDelegationsOutcome = "completed" | "timeout" | "error";

export type WaitDelegationsBaseResult = {
  delegations: DelegationSummary[];
  completed: DelegationSummary[];
  pending: DelegationSummary[];
  revision: number | null;
  serverInstanceId: string | null;
};

export type WaitDelegationsSuccessResult = WaitDelegationsBaseResult & {
  outcome: "completed" | "timeout";
  error?: never;
};

export type WaitDelegationsErrorResult = WaitDelegationsBaseResult & {
  outcome: "error";
  error: WaitDelegationErrorPacket;
};

export type WaitDelegationsResult =
  | WaitDelegationsSuccessResult
  | WaitDelegationsErrorResult;

export type WaitDelegationsOptions = {
  pollIntervalMs?: number;
  timeoutMs?: number;
};

export function delegationStatusIsTerminal(status: DelegationStatus) {
  return status === "completed" || status === "failed" || status === "canceled";
}

export async function spawnDelegationCommand(
  parentSessionId: string,
  request: CreateDelegationRequest,
): Promise<SpawnDelegationCommandResult> {
  return spawnDelegationWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    request,
  );
}

async function spawnDelegationWithTransport(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  request: CreateDelegationRequest,
): Promise<SpawnDelegationCommandResult> {
  let normalizedParentSessionId: string;
  let compactedRequest: CreateDelegationRequest;
  try {
    normalizedParentSessionId = normalizeTransportId(
      parentSessionId,
      "parent session id",
    );
    compactedRequest = compactCreateDelegationRequest(request);
  } catch (error) {
    return spawnDelegationCommandValidationErrorResult(error);
  }
  try {
    const response = await transport.createDelegation(
      normalizedParentSessionId,
      compactedRequest,
    );
    return spawnDelegationCommandResult(response);
  } catch (error) {
    return spawnDelegationCommandErrorResult(error, normalizedParentSessionId);
  }
}

export async function spawnReviewerBatchCommand(
  parentSessionId: string,
  requests: readonly SpawnReviewerBatchItem[],
): Promise<SpawnReviewerBatchCommandResult> {
  return spawnReviewerBatchWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    requests,
  );
}

async function spawnReviewerBatchWithTransport(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  requests: readonly SpawnReviewerBatchItem[],
): Promise<SpawnReviewerBatchCommandResult> {
  let normalizedParentSessionId: string;
  let normalizedRequests: ReturnType<typeof normalizeReviewerBatchRequests>;
  try {
    normalizedParentSessionId = normalizeTransportId(
      parentSessionId,
      "parent session id",
    );
    normalizedRequests = normalizeReviewerBatchRequests(requests);
  } catch (error) {
    return spawnReviewerBatchValidationErrorResult(error);
  }
  const settled = await Promise.all(
    normalizedRequests.map(({ request, title }, index) =>
      transport.createDelegation(normalizedParentSessionId, request).then(
        (response) =>
          ({
            kind: "spawned",
            result: spawnDelegationCommandResult(response),
          }) as const,
        (error: unknown) =>
          ({
            kind: "failed",
            failure: reviewerBatchFailure(
              index,
              title,
              error,
              normalizedParentSessionId,
            ),
          }) as const,
      ),
    ),
  );
  const spawned: SpawnDelegationCommandSuccessResult[] = [];
  const failed: SpawnReviewerBatchFailure[] = [];
  settled.forEach((entry) => {
    if (entry.kind === "spawned") {
      spawned.push(entry.result);
    } else {
      failed.push(entry.failure);
    }
  });
  const mixedServerInstanceError = mixedSpawnServerInstanceError(spawned);
  const metadata = mixedServerInstanceError
    ? { revision: null, serverInstanceId: null }
    : newestSpawnMetadata(spawned);
  const base = {
    spawned,
    failed,
    delegationIds: spawned.map((result) => result.delegationId),
    childSessionIds: spawned.map((result) => result.childSessionId),
    revision: metadata.revision,
    serverInstanceId: metadata.serverInstanceId,
  };
  if (mixedServerInstanceError) {
    return {
      ...base,
      outcome: "error",
      error: mixedServerInstanceError,
    };
  }
  if (failed.length === 0) {
    return {
      ...base,
      outcome: "completed",
    };
  }
  if (spawned.length > 0) {
    return {
      ...base,
      outcome: "partial",
    };
  }
  return {
    ...base,
    outcome: "error",
    error: allReviewerSpawnsFailedError(),
  };
}

function spawnDelegationCommandResult(
  response: DelegationResponse,
): SpawnDelegationCommandSuccessResult {
  return {
    outcome: "completed",
    delegationId: response.delegation.id,
    childSessionId: response.delegation.childSessionId,
    delegation: delegationSummary(response.delegation),
    childSession: delegationChildSessionSummary(response.childSession),
    revision: response.revision,
    serverInstanceId: response.serverInstanceId,
  };
}

function spawnDelegationCommandErrorResult(
  error: unknown,
  parentSessionId: string,
): SpawnDelegationCommandErrorResult {
  return {
    outcome: "error",
    revision: null,
    serverInstanceId: null,
    error: spawnDelegationFailurePacket(error, { parentSessionId }),
  };
}

function spawnDelegationCommandValidationErrorResult(
  error: unknown,
): SpawnDelegationCommandErrorResult {
  return {
    outcome: "error",
    revision: null,
    serverInstanceId: null,
    error: spawnDelegationValidationFailurePacket(error),
  };
}

function spawnReviewerBatchValidationErrorResult(
  error: unknown,
): SpawnReviewerBatchErrorResult {
  return {
    outcome: "error",
    spawned: [],
    failed: [],
    delegationIds: [],
    childSessionIds: [],
    revision: null,
    serverInstanceId: null,
    error: spawnDelegationValidationFailurePacket(error),
  };
}

export async function getDelegationStatusCommand(
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationStatusCommandResult> {
  return getDelegationStatusWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    delegationId,
  );
}

async function getDelegationStatusWithTransport(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationStatusCommandResult> {
  const response = await transport.fetchDelegationStatus(
    normalizeTransportId(parentSessionId, "parent session id"),
    normalizeTransportId(delegationId, "delegation id"),
  );
  return delegationStatusCommandResult(response);
}

export async function getDelegationResultCommand(
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationResultPacket> {
  return getDelegationResultWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    delegationId,
  );
}

async function getDelegationResultWithTransport(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationResultPacket> {
  const response = await transport.fetchDelegationResult(
    normalizeTransportId(parentSessionId, "parent session id"),
    normalizeTransportId(delegationId, "delegation id"),
  );
  return delegationResultPacket(response.result, {
    revision: response.revision,
    serverInstanceId: response.serverInstanceId,
  });
}

export async function cancelDelegationCommand(
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationStatusCommandResult> {
  return cancelDelegationWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    delegationId,
  );
}

async function cancelDelegationWithTransport(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationStatusCommandResult> {
  const response = await transport.cancelDelegation(
    normalizeTransportId(parentSessionId, "parent session id"),
    normalizeTransportId(delegationId, "delegation id"),
  );
  return delegationStatusCommandResult(response);
}

export async function waitDelegationCommand(
  parentSessionId: string,
  delegationId: string,
  options?: WaitDelegationsOptions,
): Promise<WaitDelegationsResult> {
  return waitDelegationsWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    [delegationId],
    options,
  );
}

/**
 * Polls delegation status until every requested delegation is terminal, the
 * deadline expires, or a recoverable polling error occurs. Invalid input throws
 * `RangeError` before polling starts. Once polling starts, status fetch failures
 * return `outcome: "error"` with `kind: "status-fetch-failed"`, response id
 * mismatches return `kind: "mismatched-delegation-id"`, and mixed server
 * instance batches return `kind: "mixed-server-instance"`. Error outcomes keep
 * the partial delegation/completed/pending state observed before the failure.
 */
export async function waitDelegationsCommand(
  parentSessionId: string,
  delegationIds: readonly string[],
  options: WaitDelegationsOptions = {},
): Promise<WaitDelegationsResult> {
  return waitDelegationsWithTransport(
    browserDelegationCommandTransport,
    parentSessionId,
    delegationIds,
    options,
  );
}

async function waitDelegationsWithTransport(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  delegationIds: readonly string[],
  options: WaitDelegationsOptions = {},
): Promise<WaitDelegationsResult> {
  const normalizedParentSessionId = normalizeTransportId(
    parentSessionId,
    "parent session id",
  );
  const ids = normalizeDelegationIds(delegationIds);
  if (ids.length === 0) {
    throw new RangeError(
      "wait_delegations requires at least one delegation id",
    );
  }
  if (ids.length > MAX_DELEGATION_WAIT_IDS) {
    throw new RangeError(
      `wait_delegations accepts at most ${MAX_DELEGATION_WAIT_IDS} ids`,
    );
  }
  const pollIntervalMs = normalizedFinitePositiveDuration(
    options.pollIntervalMs,
    DEFAULT_DELEGATION_WAIT_INTERVAL_MS,
    "pollIntervalMs",
  );
  if (pollIntervalMs < MIN_DELEGATION_WAIT_INTERVAL_MS) {
    throw new RangeError(
      `pollIntervalMs must be at least ${MIN_DELEGATION_WAIT_INTERVAL_MS}ms`,
    );
  }
  const timeoutMs = normalizedFinitePositiveDuration(
    options.timeoutMs,
    DEFAULT_DELEGATION_WAIT_TIMEOUT_MS,
    "timeoutMs",
  );
  if (timeoutMs > MAX_DELEGATION_WAIT_TIMEOUT_MS) {
    throw new RangeError(
      `timeoutMs must be no greater than ${MAX_DELEGATION_WAIT_TIMEOUT_MS}ms`,
    );
  }
  const deadlineAt = Date.now() + timeoutMs;
  const recordsById = new Map<string, DelegationRecord>();
  const revisionByServerInstanceId = new Map<string, number>();
  let lastRevision: number | null = null;
  let lastServerInstanceId: string | null = null;

  for (;;) {
    const pendingIds = ids.filter((id) => {
      const record = recordsById.get(id);
      return record === undefined || !delegationStatusIsTerminal(record.status);
    });

    const remainingMs = deadlineAt - Date.now();
    if (remainingMs <= 0) {
      return waitDelegationsResult(
        "timeout",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
      );
    }
    const batchTimeoutMs = Math.min(pollIntervalMs * 2, remainingMs);
    const batch = await fetchStatusBatchWithDeadline(
      transport,
      normalizedParentSessionId,
      pendingIds,
      batchTimeoutMs,
    );

    if (batch.kind === "error") {
      const appliedResponses = applyCurrentInstanceStatusBatchResponses(
        batch.responses,
        recordsById,
        lastServerInstanceId,
      );
      recordStatusBatchMetadata(appliedResponses, revisionByServerInstanceId);
      const metadata = newestStatusMetadata(
        appliedResponses,
        lastRevision,
        lastServerInstanceId,
      );
      return waitDelegationsResult(
        "error",
        ids,
        recordsById,
        metadata.revision,
        metadata.serverInstanceId,
        statusFetchErrorPacket(batch.error, {
          parentSessionId: normalizedParentSessionId,
          delegationId: batch.requestedId,
        }),
      );
    }

    const applyResult = applyStatusBatchResponses(
      batch.responses,
      recordsById,
      serverInstanceRevision(revisionByServerInstanceId, lastServerInstanceId),
      lastServerInstanceId,
    );
    if (applyResult.error) {
      return waitDelegationsResult(
        "error",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
        applyResult.error,
      );
    }
    const metadata = newestStatusMetadata(
      applyResult.appliedResponses,
      lastRevision,
      lastServerInstanceId,
    );
    recordStatusBatchMetadata(
      applyResult.appliedResponses,
      revisionByServerInstanceId,
    );
    lastRevision = metadata.revision;
    lastServerInstanceId = metadata.serverInstanceId;

    if (batch.kind === "timeout") {
      if (deadlineAt - Date.now() <= 0) {
        return waitDelegationsResult(
          "timeout",
          ids,
          recordsById,
          lastRevision,
          lastServerInstanceId,
        );
      }
      await delay(Math.min(pollIntervalMs, deadlineAt - Date.now()));
      continue;
    }
    if (batch.kind === "responses" && batch.responses.length === 0) {
      return waitDelegationsResult(
        "completed",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
      );
    }

    const pending = ids
      .map((id) => recordsById.get(id))
      .filter(
        (record): record is DelegationRecord =>
          record !== undefined && !delegationStatusIsTerminal(record.status),
      );
    if (pending.length === 0 && recordsById.size === ids.length) {
      return waitDelegationsResult(
        "completed",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
      );
    }

    const waitMs = Math.min(pollIntervalMs, deadlineAt - Date.now());
    if (waitMs <= 0) {
      return waitDelegationsResult(
        "timeout",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
      );
    }

    await delay(waitMs);
  }
}

export function createDelegationCommands(
  transport: DelegationCommandTransport = browserDelegationCommandTransport,
) {
  return {
    spawn_delegation: (
      parentSessionId: string,
      request: CreateDelegationRequest,
    ) => spawnDelegationWithTransport(transport, parentSessionId, request),
    spawn_reviewer_batch: (
      parentSessionId: string,
      requests: readonly SpawnReviewerBatchItem[],
    ) => spawnReviewerBatchWithTransport(transport, parentSessionId, requests),
    get_delegation_status: (parentSessionId: string, delegationId: string) =>
      getDelegationStatusWithTransport(
        transport,
        parentSessionId,
        delegationId,
      ),
    get_delegation_result: (parentSessionId: string, delegationId: string) =>
      getDelegationResultWithTransport(
        transport,
        parentSessionId,
        delegationId,
      ),
    cancel_delegation: (parentSessionId: string, delegationId: string) =>
      cancelDelegationWithTransport(transport, parentSessionId, delegationId),
    wait_delegations: (
      parentSessionId: string,
      delegationIds: readonly string[],
      options?: WaitDelegationsOptions,
    ) =>
      waitDelegationsWithTransport(
        transport,
        parentSessionId,
        delegationIds,
        options,
      ),
  } as const;
}

export const delegationCommands = createDelegationCommands();

function delegationStatusCommandResult(response: {
  delegation: DelegationRecord;
  revision: number;
  serverInstanceId: string;
}): DelegationStatusCommandResult {
  return {
    delegationId: response.delegation.id,
    childSessionId: response.delegation.childSessionId,
    status: response.delegation.status,
    delegation: delegationSummary(response.delegation),
    revision: response.revision,
    serverInstanceId: response.serverInstanceId,
  };
}

function delegationResultPacket(
  result: DelegationResult,
  metadata: { revision: number; serverInstanceId: string },
): DelegationResultPacket {
  return {
    delegationId: result.delegationId,
    childSessionId: result.childSessionId,
    status: result.status,
    summary: result.summary,
    findings: result.findings ?? [],
    changedFiles: result.changedFiles ?? [],
    commandsRun: result.commandsRun ?? [],
    notes: result.notes ?? [],
    revision: metadata.revision,
    serverInstanceId: metadata.serverInstanceId,
  };
}

function delegationSummary(record: DelegationRecord): DelegationSummary {
  const summary: DelegationSummary = {
    id: record.id,
    parentSessionId: record.parentSessionId,
    childSessionId: record.childSessionId,
    mode: record.mode,
    status: record.status,
    title: record.title,
    agent: record.agent,
    model: record.model ?? null,
    writePolicy: record.writePolicy,
    createdAt: record.createdAt,
    startedAt: record.startedAt ?? null,
    completedAt: record.completedAt ?? null,
  };
  if (record.result) {
    summary.result = {
      delegationId: record.result.delegationId,
      childSessionId: record.result.childSessionId,
      status: record.result.status,
      summary: record.result.summary,
    };
  }
  return summary;
}

function delegationChildSessionSummary(
  session: Session,
): DelegationChildSessionSummary {
  return {
    id: session.id,
    name: session.name,
    emoji: session.emoji,
    agent: session.agent,
    model: session.model,
    status: session.status,
    parentDelegationId: session.parentDelegationId ?? null,
  };
}

function waitDelegationsResult(
  outcome: WaitDelegationsOutcome,
  ids: readonly string[],
  recordsById: ReadonlyMap<string, DelegationRecord>,
  revision: number | null,
  serverInstanceId: string | null,
  error?: WaitDelegationErrorPacket,
): WaitDelegationsResult {
  const delegations = ids
    .map((id) => recordsById.get(id))
    .filter((record): record is DelegationRecord => record !== undefined)
    .map(delegationSummary);
  const completed = delegations.filter((record) =>
    delegationStatusIsTerminal(record.status),
  );
  const pending = delegations.filter(
    (record) => !delegationStatusIsTerminal(record.status),
  );
  const base = {
    delegations,
    completed,
    pending,
    revision,
    serverInstanceId,
  };
  if (outcome === "error") {
    if (!error) {
      throw new Error(
        "waitDelegationsResult error outcomes require an error packet",
      );
    }
    return {
      ...base,
      outcome,
      error,
    };
  }
  return {
    ...base,
    outcome,
  };
}

type StatusBatchResponse = {
  requestedId: string;
  requestedIndex: number;
  response: DelegationStatusResponse;
};

type StatusBatchResult =
  | { kind: "responses"; responses: StatusBatchResponse[] }
  | { kind: "timeout"; responses: StatusBatchResponse[] }
  | {
      kind: "error";
      error: unknown;
      requestedId: string;
      responses: StatusBatchResponse[];
    };

type StatusBatchFinish =
  | { kind: "responses" }
  | { kind: "timeout" }
  | { kind: "error"; error: unknown; requestedId: string };

type StatusBatchApplyResult = {
  appliedResponses: StatusBatchResponse[];
  error: WaitDelegationErrorPacket | null;
};

async function fetchStatusBatchWithDeadline(
  transport: DelegationCommandTransport,
  parentSessionId: string,
  pendingIds: readonly string[],
  remainingMs: number,
): Promise<StatusBatchResult> {
  if (remainingMs <= 0) {
    return { kind: "timeout", responses: [] };
  }
  const controller = new AbortController();
  let timeoutId: ReturnType<typeof globalThis.setTimeout> | undefined;
  let finished = false;
  let completedCount = 0;
  const responses: StatusBatchResponse[] = [];

  return new Promise<StatusBatchResult>((resolve) => {
    const finish = (
      result: StatusBatchFinish,
      options: { abortPending?: boolean } = {},
    ) => {
      if (finished) {
        return;
      }
      finished = true;
      if (timeoutId !== undefined) {
        globalThis.clearTimeout(timeoutId);
      }
      if (options.abortPending) {
        controller.abort();
      }
      const responseSnapshot = [...responses];
      if (result.kind === "error") {
        resolve({
          kind: "error",
          error: result.error,
          requestedId: result.requestedId,
          responses: responseSnapshot,
        });
        return;
      }
      resolve({ kind: result.kind, responses: responseSnapshot });
    };

    if (pendingIds.length === 0) {
      finish({ kind: "responses" });
      return;
    }

    timeoutId = globalThis.setTimeout(() => {
      finish({ kind: "timeout" }, { abortPending: true });
    }, remainingMs);

    for (const [requestedIndex, requestedId] of pendingIds.entries()) {
      transport
        .fetchDelegationStatus(parentSessionId, requestedId, {
          signal: controller.signal,
        })
        .then(
          (response) => {
            if (finished) {
              return;
            }
            responses.push({ requestedId, requestedIndex, response });
            completedCount += 1;
            if (completedCount === pendingIds.length) {
              finish({ kind: "responses" });
            }
          },
          (error: unknown) => {
            finish(
              { kind: "error", error, requestedId },
              { abortPending: true },
            );
          },
        );
    }
  });
}

function applyStatusBatchResponses(
  responses: readonly StatusBatchResponse[],
  recordsById: Map<string, DelegationRecord>,
  previousServerInstanceRevision: number | null,
  previousServerInstanceId: string | null,
): StatusBatchApplyResult {
  for (const { requestedId, response } of responses) {
    if (response.delegation.id !== requestedId) {
      return {
        appliedResponses: [],
        error: mismatchedDelegationIdErrorPacket(
          requestedId,
          response.delegation.id,
        ),
      };
    }
  }

  const serverInstanceIds = mixedServerInstanceIds(
    responses.map(({ response }) => response),
    previousServerInstanceId,
  );
  if (serverInstanceIds.length > 1) {
    return {
      appliedResponses: [],
      error: mixedServerInstanceErrorPacket(serverInstanceIds, {
        operation: "status-batch",
        recoveryGroups: statusRecoveryGroups(
          responses,
          recordsById,
          previousServerInstanceRevision,
          previousServerInstanceId,
        ),
      }),
    };
  }

  for (const { requestedId, response } of responses) {
    recordsById.set(requestedId, response.delegation);
  }
  return {
    appliedResponses: [...responses],
    error: null,
  };
}

function statusRecoveryGroups(
  responses: readonly StatusBatchResponse[],
  recordsById: ReadonlyMap<string, DelegationRecord>,
  previousServerInstanceRevision: number | null,
  previousServerInstanceId: string | null,
): MixedServerInstanceRecoveryGroup[] {
  const requestedOrder = new Map(
    responses.map((response) => [
      response.requestedId,
      response.requestedIndex,
    ]),
  );
  const groups = new Map<string, MixedServerInstanceRecoveryGroup>();
  if (previousServerInstanceId && previousServerInstanceRevision !== null) {
    const requestedIds = new Set(
      responses.map((response) => response.requestedId),
    );
    const previousRecords = [...recordsById.values()].filter((record) =>
      requestedIds.has(record.id),
    );
    if (previousRecords.length > 0) {
      groups.set(previousServerInstanceId, {
        serverInstanceId: previousServerInstanceId,
        revision: previousServerInstanceRevision,
        delegationIds: previousRecords.map((record) => record.id),
        childSessionIds: previousRecords.map((record) => record.childSessionId),
      });
    }
  }
  for (const { response } of responses) {
    const serverInstanceId = response.serverInstanceId;
    const delegationId = response.delegation.id;
    const childSessionId = response.delegation.childSessionId;
    const group = groups.get(serverInstanceId);
    if (group) {
      group.revision = Math.max(group.revision, response.revision);
      upsertRecoveryGroupDelegation(group, delegationId, childSessionId);
      continue;
    }
    groups.set(serverInstanceId, {
      serverInstanceId,
      revision: response.revision,
      delegationIds: [delegationId],
      childSessionIds: [childSessionId],
    });
  }
  return sortedStatusRecoveryGroups(groups, requestedOrder);
}

function recordStatusBatchMetadata(
  responses: readonly StatusBatchResponse[],
  revisionByServerInstanceId: Map<string, number>,
) {
  for (const { response } of responses) {
    const previousRevision = revisionByServerInstanceId.get(
      response.serverInstanceId,
    );
    if (
      previousRevision === undefined ||
      response.revision > previousRevision
    ) {
      revisionByServerInstanceId.set(
        response.serverInstanceId,
        response.revision,
      );
    }
  }
}

function serverInstanceRevision(
  revisionByServerInstanceId: ReadonlyMap<string, number>,
  serverInstanceId: string | null,
) {
  return serverInstanceId === null
    ? null
    : (revisionByServerInstanceId.get(serverInstanceId) ?? null);
}

function applyCurrentInstanceStatusBatchResponses(
  responses: readonly StatusBatchResponse[],
  recordsById: Map<string, DelegationRecord>,
  previousServerInstanceId: string | null,
) {
  const referenceServerInstanceId =
    previousServerInstanceId ?? singleServerInstanceId(responses);
  if (!referenceServerInstanceId) {
    return [];
  }

  const appliedResponses: StatusBatchResponse[] = [];
  for (const response of responses) {
    if (
      response.response.serverInstanceId !== referenceServerInstanceId ||
      response.response.delegation.id !== response.requestedId
    ) {
      continue;
    }
    recordsById.set(response.requestedId, response.response.delegation);
    appliedResponses.push(response);
  }
  return appliedResponses;
}

function singleServerInstanceId(responses: readonly StatusBatchResponse[]) {
  const serverInstanceIds = new Set(
    responses.map(({ response }) => response.serverInstanceId),
  );
  return serverInstanceIds.size === 1 ? [...serverInstanceIds][0] : null;
}

function newestStatusMetadata(
  responses: readonly StatusBatchResponse[],
  previousRevision: number | null,
  previousServerInstanceId: string | null,
) {
  let revision = previousRevision;
  let serverInstanceId = previousServerInstanceId;
  for (const { response } of responses) {
    if (revision === null || response.revision > revision) {
      revision = response.revision;
      serverInstanceId = response.serverInstanceId;
    }
  }
  return { revision, serverInstanceId };
}

function mixedServerInstanceIds(
  responses: readonly DelegationStatusResponse[],
  previousServerInstanceId: string | null,
) {
  const serverInstanceIds = new Set<string>();
  if (previousServerInstanceId) {
    serverInstanceIds.add(previousServerInstanceId);
  }
  responses.forEach((response) => {
    serverInstanceIds.add(response.serverInstanceId);
  });
  return [...serverInstanceIds];
}

function normalizeDelegationIds(delegationIds: readonly string[]) {
  if (!Array.isArray(delegationIds)) {
    throw new TypeError("delegation ids must be an array");
  }
  const seen = new Set<string>();
  const normalizedIds: string[] = [];
  for (const id of delegationIds) {
    const normalized = normalizeTransportId(id, "delegation id");
    if (!seen.has(normalized)) {
      seen.add(normalized);
      normalizedIds.push(normalized);
    }
  }
  return normalizedIds;
}

function normalizeReviewerBatchRequests(
  requests: readonly SpawnReviewerBatchItem[],
) {
  if (!Array.isArray(requests)) {
    throw new TypeError("spawn_reviewer_batch requests must be an array");
  }
  if (requests.length === 0) {
    throw new RangeError("spawn_reviewer_batch requires at least one reviewer");
  }
  if (requests.length > MAX_REVIEWER_BATCH_SIZE) {
    throw new RangeError(
      `spawn_reviewer_batch accepts at most ${MAX_REVIEWER_BATCH_SIZE} reviewers`,
    );
  }
  return requests.map((request, index) => {
    if (request === null || typeof request !== "object") {
      throw new TypeError(`reviewer request ${index + 1} must be an object`);
    }
    const compacted = compactReviewerBatchRequest({
      ...request,
      mode: "reviewer",
      writePolicy: { kind: "readOnly" },
    });
    return {
      request: compacted,
      title: reviewerBatchItemTitle(compacted),
    };
  });
}

function reviewerBatchItemTitle(request: CreateDelegationRequest) {
  const title = typeof request.title === "string" ? request.title.trim() : "";
  return title.length > 0 ? title : null;
}

function reviewerBatchFailure(
  index: number,
  title: string | null,
  error: unknown,
  parentSessionId: string,
): SpawnReviewerBatchFailure {
  return {
    index,
    title,
    ...spawnDelegationFailurePacket(error, { parentSessionId }),
  };
}

function compactReviewerBatchRequest(
  request: CreateDelegationRequest,
): CreateDelegationRequest {
  return compactCreateDelegationRequest(request);
}

function mixedSpawnServerInstanceError(
  spawned: readonly SpawnDelegationCommandSuccessResult[],
) {
  const serverInstanceIds = spawned.map((result) => result.serverInstanceId);
  const uniqueServerInstanceIds = new Set(serverInstanceIds);
  return uniqueServerInstanceIds.size > 1
    ? mixedServerInstanceErrorPacket(serverInstanceIds, {
        operation: "spawn-batch",
        recoveryGroups: spawnRecoveryGroups(spawned),
      })
    : null;
}

function spawnRecoveryGroups(
  spawned: readonly SpawnDelegationCommandSuccessResult[],
): MixedServerInstanceRecoveryGroup[] {
  const groups = new Map<string, MixedServerInstanceRecoveryGroup>();
  for (const result of spawned) {
    const group = groups.get(result.serverInstanceId);
    if (group) {
      group.revision = Math.max(group.revision, result.revision);
      pushUnique(group.delegationIds, result.delegationId);
      pushUnique(group.childSessionIds, result.childSessionId);
      continue;
    }
    groups.set(result.serverInstanceId, {
      serverInstanceId: result.serverInstanceId,
      revision: result.revision,
      delegationIds: [result.delegationId],
      childSessionIds: [result.childSessionId],
    });
  }
  return sortedRecoveryGroups(groups);
}

function allReviewerSpawnsFailedError(): SpawnReviewerBatchErrorPacket {
  return {
    kind: "all-spawns-failed",
    name: "SpawnReviewerBatchError",
    message: "all reviewer spawns failed",
  };
}

function pushUnique(values: string[], value: string) {
  if (!values.includes(value)) {
    values.push(value);
  }
}

function sortedRecoveryGroups(
  groups: ReadonlyMap<string, MixedServerInstanceRecoveryGroup>,
) {
  return [...groups.values()].sort((left, right) =>
    left.serverInstanceId.localeCompare(right.serverInstanceId),
  );
}

function sortedStatusRecoveryGroups(
  groups: ReadonlyMap<string, MixedServerInstanceRecoveryGroup>,
  requestedOrder: ReadonlyMap<string, number>,
) {
  return [...groups.values()]
    .map((group) => recoveryGroupSortedByRequestOrder(group, requestedOrder))
    .sort((left, right) =>
      compareRecoveryGroupsByRequestOrder(left, right, requestedOrder),
    );
}

function recoveryGroupSortedByRequestOrder(
  group: MixedServerInstanceRecoveryGroup,
  requestedOrder: ReadonlyMap<string, number>,
) {
  const pairs = group.delegationIds.map((delegationId, index) => {
    const childSessionId = group.childSessionIds[index];
    if (childSessionId === undefined) {
      throw new Error(
        `recovery group missing child session id for ${delegationId}`,
      );
    }
    return { delegationId, childSessionId };
  });
  pairs.sort((left, right) =>
    compareRequestedDelegationIds(
      left.delegationId,
      right.delegationId,
      requestedOrder,
    ),
  );
  return {
    ...group,
    delegationIds: pairs.map((pair) => pair.delegationId),
    childSessionIds: pairs.map((pair) => pair.childSessionId),
  };
}

function compareRequestedDelegationIds(
  left: string,
  right: string,
  requestedOrder: ReadonlyMap<string, number>,
) {
  const leftOrder = requestedOrderIndex(left, requestedOrder);
  const rightOrder = requestedOrderIndex(right, requestedOrder);
  if (leftOrder !== rightOrder) {
    return leftOrder - rightOrder;
  }
  return left.localeCompare(right);
}

function compareRecoveryGroupsByRequestOrder(
  left: MixedServerInstanceRecoveryGroup,
  right: MixedServerInstanceRecoveryGroup,
  requestedOrder: ReadonlyMap<string, number>,
) {
  const leftOrder = recoveryGroupFirstRequestedIndex(left, requestedOrder);
  const rightOrder = recoveryGroupFirstRequestedIndex(right, requestedOrder);
  if (leftOrder !== rightOrder) {
    return leftOrder - rightOrder;
  }
  return left.serverInstanceId.localeCompare(right.serverInstanceId);
}

function recoveryGroupFirstRequestedIndex(
  group: MixedServerInstanceRecoveryGroup,
  requestedOrder: ReadonlyMap<string, number>,
) {
  return Math.min(
    ...group.delegationIds.map((delegationId) =>
      requestedOrderIndex(delegationId, requestedOrder),
    ),
  );
}

function requestedOrderIndex(
  delegationId: string,
  requestedOrder: ReadonlyMap<string, number>,
) {
  const index = requestedOrder.get(delegationId);
  if (index === undefined) {
    throw new Error(
      `missing requested order for delegation id ${delegationId}`,
    );
  }
  return index;
}

function upsertRecoveryGroupDelegation(
  group: MixedServerInstanceRecoveryGroup,
  delegationId: string,
  childSessionId: string,
) {
  const existingIndex = group.delegationIds.indexOf(delegationId);
  if (existingIndex >= 0) {
    group.childSessionIds[existingIndex] = childSessionId;
    return;
  }
  group.delegationIds.push(delegationId);
  group.childSessionIds.push(childSessionId);
}

function newestSpawnMetadata(
  spawned: readonly SpawnDelegationCommandSuccessResult[],
) {
  let revision: number | null = null;
  const serverInstanceIds = new Set<string>();
  for (const result of spawned) {
    if (revision === null || result.revision > revision) {
      revision = result.revision;
    }
    serverInstanceIds.add(result.serverInstanceId);
  }
  return {
    revision,
    serverInstanceId:
      serverInstanceIds.size === 1 ? [...serverInstanceIds][0] : null,
  };
}

function normalizedFinitePositiveDuration(
  value: number | undefined,
  fallback: number,
  label: string,
) {
  const duration = value ?? fallback;
  if (!Number.isFinite(duration) || duration <= 0) {
    throw new RangeError(`${label} must be a finite positive duration`);
  }
  return duration;
}

function normalizeTransportId(value: string, label: string) {
  if (typeof value !== "string") {
    throw new TypeError(`${label} must be a string`);
  }
  const normalized = value.trim();
  if (!normalized) {
    throw new RangeError(`${label} must be non-empty`);
  }
  if (UNSAFE_TRANSPORT_ID_PATTERN.test(normalized)) {
    throw new RangeError(
      `${label} must not contain /, ?, #, or control characters`,
    );
  }
  return normalized;
}

function compactCreateDelegationRequest(
  request: CreateDelegationRequest,
): CreateDelegationRequest {
  const prompt = normalizeDelegationPrompt(request.prompt);
  const payload: Record<string, unknown> = {
    prompt,
  };
  for (const [key, value] of Object.entries(request)) {
    if (key === "prompt" || value === undefined) {
      continue;
    }
    if (value === null) {
      throw new TypeError(`${key} must be omitted instead of null`);
    }
    if (key === "title" && typeof value === "string") {
      const title = value.trim();
      validateDelegationMetadataText(
        title,
        "title",
        MAX_DELEGATION_TITLE_CHARS,
      );
      if (title.length > 0) {
        payload[key] = title;
      }
      continue;
    }
    if (key === "model" && typeof value === "string") {
      validateDelegationMetadataText(
        value,
        "model",
        MAX_DELEGATION_MODEL_CHARS,
      );
    }
    payload[key] = value;
  }
  return payload as CreateDelegationRequest;
}

function validateDelegationMetadataText(
  value: string,
  label: string,
  maxChars: number,
) {
  const textLength = Array.from(value.trim()).length;
  if (textLength > maxChars) {
    throw new RangeError(
      `${label} must be no longer than ${maxChars} characters`,
    );
  }
}

function normalizeDelegationPrompt(prompt: string) {
  if (typeof prompt !== "string") {
    throw new TypeError("prompt must be a string");
  }
  const normalizedPrompt = prompt.trim();
  if (!normalizedPrompt) {
    throw new RangeError("prompt must be non-empty");
  }
  const promptByteLength = new TextEncoder().encode(
    normalizedPrompt,
  ).byteLength;
  if (promptByteLength > MAX_DELEGATION_PROMPT_BYTES) {
    throw new RangeError(
      `prompt must be no larger than ${MAX_DELEGATION_PROMPT_BYTES} bytes`,
    );
  }
  return normalizedPrompt;
}

function delay(ms: number) {
  return new Promise<void>((resolve) => {
    globalThis.setTimeout(resolve, ms);
  });
}

// Owns the Phase 2 delegation command surface that UI/MCP wrappers can bind to:
// spawn, status, result, cancel, and polling wait helpers. Does not own route
// transport, live SSE adoption, or backend delegation lifecycle rules. New
// module staged ahead of the TermAl MCP wrapper integration.

import {
  ApiRequestError,
  cancelDelegation,
  createDelegation,
  fetchDelegationResult,
  fetchDelegationStatus,
  type AbortableRequestOptions,
  type ApiRequestErrorKind,
  type CreateDelegationRequest,
  type DelegationResponse,
  type DelegationResultResponse,
  type DelegationStatusResponse,
} from "./api";
import type {
  DelegationCommandResult,
  DelegationFinding,
  DelegationRecord,
  DelegationResult,
  DelegationSummary,
  DelegationStatus,
  Session,
} from "./types";

export const DEFAULT_DELEGATION_WAIT_INTERVAL_MS = 1000;
export const DEFAULT_DELEGATION_WAIT_TIMEOUT_MS = 5 * 60 * 1000;
export const MIN_DELEGATION_WAIT_INTERVAL_MS = 500;
export const MAX_DELEGATION_WAIT_TIMEOUT_MS = 30 * 60 * 1000;
export const MAX_DELEGATION_WAIT_IDS = 10;
export const MAX_DELEGATION_PROMPT_CHARS = 256 * 1024;
const UNSAFE_TRANSPORT_ID_PATTERN = /[/?#\u0000-\u001f\u007f]/u;

// These limits compose to at most 20 status requests/sec per wait call. The MCP
// wrapper should still add a process-level concurrency cap before exposing this
// surface to untrusted callers.

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

export type SpawnDelegationCommandResult = {
  delegationId: string;
  childSessionId: string;
  delegation: DelegationRecord;
  childSession: Session;
  revision: number;
  serverInstanceId: string;
};

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

export type WaitDelegationErrorPacket =
  | {
      kind: "mismatched-delegation-id";
      name: string;
      message: string;
      requestedId: string;
      receivedId: string;
    }
  | {
      kind: "mixed-server-instance";
      name: string;
      message: string;
      serverInstanceIds: string[];
    }
  | {
      kind: "status-fetch-failed";
      name: string;
      message: string;
      apiErrorKind?: ApiRequestErrorKind;
      status?: number | null;
      restartRequired?: boolean;
    };

export type WaitDelegationErrorKind = WaitDelegationErrorPacket["kind"];

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
  const normalizedParentSessionId = normalizeTransportId(
    parentSessionId,
    "parent session id",
  );
  const response = await transport.createDelegation(
    normalizedParentSessionId,
    compactCreateDelegationRequest(request),
  );
  return {
    delegationId: response.delegation.id,
    childSessionId: response.delegation.childSessionId,
    delegation: response.delegation,
    childSession: response.childSession,
    revision: response.revision,
    serverInstanceId: response.serverInstanceId,
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
    throw new RangeError("wait_delegations requires at least one delegation id");
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
  let lastRevision: number | null = null;
  let lastServerInstanceId: string | null = null;

  for (;;) {
    const pendingIds = ids.filter((id) => {
      const record = recordsById.get(id);
      return record === undefined || !delegationStatusIsTerminal(record.status);
    });

    const remainingMs = deadlineAt - Date.now();
    const batch = await fetchStatusBatchWithDeadline(
      transport,
      normalizedParentSessionId,
      pendingIds,
      remainingMs,
    );

    const responseError = applyStatusBatchResponses(
      batch.responses,
      recordsById,
      lastServerInstanceId,
    );
    if (responseError) {
      return waitDelegationsResult(
        "error",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
        responseError,
      );
    }
    for (const { response } of batch.responses) {
      if (lastRevision === null || response.revision > lastRevision) {
        lastRevision = response.revision;
        lastServerInstanceId = response.serverInstanceId;
      }
    }
    if (batch.kind === "timeout") {
      return waitDelegationsResult(
        "timeout",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
      );
    }
    if (batch.kind === "error") {
      return waitDelegationsResult(
        "error",
        ids,
        recordsById,
        lastRevision,
        lastServerInstanceId,
        statusFetchErrorPacket(batch.error),
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
    spawn_delegation: (parentSessionId: string, request: CreateDelegationRequest) =>
      spawnDelegationWithTransport(transport, parentSessionId, request),
    get_delegation_status: (parentSessionId: string, delegationId: string) =>
      getDelegationStatusWithTransport(transport, parentSessionId, delegationId),
    get_delegation_result: (parentSessionId: string, delegationId: string) =>
      getDelegationResultWithTransport(transport, parentSessionId, delegationId),
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
    model: record.model,
    writePolicy: record.writePolicy,
    createdAt: record.createdAt,
    startedAt: record.startedAt,
    completedAt: record.completedAt,
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
  response: DelegationStatusResponse;
};

type StatusBatchResult =
  | { kind: "responses"; responses: StatusBatchResponse[] }
  | { kind: "timeout"; responses: StatusBatchResponse[] }
  | { kind: "error"; error: unknown; responses: StatusBatchResponse[] };

type StatusBatchFinish =
  | { kind: "responses" }
  | { kind: "timeout" }
  | { kind: "error"; error: unknown };

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
      resolve({ ...result, responses: [...responses] } as StatusBatchResult);
    };

    if (pendingIds.length === 0) {
      finish({ kind: "responses" });
      return;
    }

    timeoutId = globalThis.setTimeout(() => {
      finish({ kind: "timeout" }, { abortPending: true });
    }, remainingMs);

    for (const requestedId of pendingIds) {
      transport
        .fetchDelegationStatus(parentSessionId, requestedId, {
          signal: controller.signal,
        })
        .then(
          (response) => {
            if (finished) {
              return;
            }
            responses.push({ requestedId, response });
            completedCount += 1;
            if (completedCount === pendingIds.length) {
              finish({ kind: "responses" });
            }
          },
          (error: unknown) => {
            finish({ kind: "error", error }, { abortPending: true });
          },
        );
    }
  });
}

function applyStatusBatchResponses(
  responses: readonly StatusBatchResponse[],
  recordsById: Map<string, DelegationRecord>,
  previousServerInstanceId: string | null,
): WaitDelegationErrorPacket | null {
  const serverInstanceIds = mixedServerInstanceIds(
    responses.map(({ response }) => response),
    previousServerInstanceId,
  );
  if (serverInstanceIds.length > 1) {
    return mixedServerInstanceErrorPacket(serverInstanceIds);
  }

  for (const { requestedId, response } of responses) {
    if (response.delegation.id !== requestedId) {
      return mismatchedDelegationIdErrorPacket(
        requestedId,
        response.delegation.id,
      );
    }
    recordsById.set(requestedId, response.delegation);
  }
  return null;
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

function statusFetchErrorPacket(error: unknown): WaitDelegationErrorPacket {
  if (error instanceof ApiRequestError) {
    return {
      kind: "status-fetch-failed",
      name: error.name,
      message: error.message,
      apiErrorKind: error.kind,
      status: error.status,
      restartRequired: error.restartRequired,
    };
  }
  const name = error instanceof Error ? error.name : "Error";
  return {
    kind: "status-fetch-failed",
    name,
    message: "Delegation status fetch failed.",
  };
}

function mismatchedDelegationIdErrorPacket(
  requestedId: string,
  receivedId: string,
): WaitDelegationErrorPacket {
  return {
    kind: "mismatched-delegation-id",
    name: "MismatchedDelegationIdError",
    message: `delegation status id mismatch: requested ${requestedId}, received ${receivedId}`,
    requestedId,
    receivedId,
  };
}

function mixedServerInstanceErrorPacket(
  serverInstanceIds: readonly string[],
): WaitDelegationErrorPacket {
  const uniqueServerInstanceIds = [...new Set(serverInstanceIds)].sort();
  return {
    kind: "mixed-server-instance",
    name: "MixedDelegationServerInstanceError",
    message: `delegation status batch contained multiple server instances: ${uniqueServerInstanceIds.join(", ")}`,
    serverInstanceIds: uniqueServerInstanceIds,
  };
}

function normalizeDelegationIds(delegationIds: readonly string[]) {
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
    payload[key] = value;
  }
  return payload as CreateDelegationRequest;
}

function normalizeDelegationPrompt(prompt: string) {
  if (typeof prompt !== "string") {
    throw new TypeError("prompt must be a string");
  }
  const normalizedPrompt = prompt.trim();
  if (!normalizedPrompt) {
    throw new RangeError("prompt must be non-empty");
  }
  if (normalizedPrompt.length > MAX_DELEGATION_PROMPT_CHARS) {
    throw new RangeError(
      `prompt must be no longer than ${MAX_DELEGATION_PROMPT_CHARS} characters`,
    );
  }
  return normalizedPrompt;
}

function delay(ms: number) {
  return new Promise<void>((resolve) => {
    globalThis.setTimeout(resolve, ms);
  });
}

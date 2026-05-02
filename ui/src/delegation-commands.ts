// Owns the Phase 2 delegation command surface that UI/MCP wrappers can bind to:
// spawn, status, result, cancel, and polling wait helpers. Does not own route
// transport, live SSE adoption, or backend delegation lifecycle rules. New
// module staged ahead of the TermAl MCP wrapper integration.

import {
  cancelDelegation,
  createDelegation,
  fetchDelegationResult,
  fetchDelegationStatus,
  type CreateDelegationRequest,
} from "./api";
import type {
  DelegationCommandResult,
  DelegationFinding,
  DelegationRecord,
  DelegationResult,
  DelegationStatus,
  Session,
} from "./types";

export const DEFAULT_DELEGATION_WAIT_INTERVAL_MS = 1000;
export const DEFAULT_DELEGATION_WAIT_TIMEOUT_MS = 5 * 60 * 1000;
export const MIN_DELEGATION_WAIT_INTERVAL_MS = 100;
export const MAX_DELEGATION_WAIT_TIMEOUT_MS = 30 * 60 * 1000;
export const MAX_DELEGATION_WAIT_IDS = 20;

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
  delegation: DelegationRecord;
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
  failedChecks: DelegationCommandResult[];
  notes: string[];
  revision: number;
  serverInstanceId: string;
};

export type WaitDelegationsOutcome = "completed" | "timeout";

export type WaitDelegationsResult = {
  outcome: WaitDelegationsOutcome;
  delegations: DelegationRecord[];
  completed: DelegationRecord[];
  pending: DelegationRecord[];
  revision: number | null;
  serverInstanceId: string | null;
};

export type WaitDelegationsOptions = {
  pollIntervalMs?: number;
  timeoutMs?: number;
};

export class MismatchedDelegationIdError extends Error {
  constructor(
    readonly requestedId: string,
    readonly receivedId: string,
  ) {
    super(
      `delegation status id mismatch: requested ${requestedId}, received ${receivedId}`,
    );
    this.name = "MismatchedDelegationIdError";
  }
}

export class MixedDelegationServerInstanceError extends Error {
  constructor(
    readonly expectedServerInstanceId: string,
    readonly receivedServerInstanceId: string,
  ) {
    super(
      `delegation status server instance changed during wait: ${expectedServerInstanceId} -> ${receivedServerInstanceId}`,
    );
    this.name = "MixedDelegationServerInstanceError";
  }
}

export function delegationStatusIsTerminal(status: DelegationStatus) {
  return status === "completed" || status === "failed" || status === "canceled";
}

export async function spawnDelegationCommand(
  parentSessionId: string,
  request: CreateDelegationRequest,
): Promise<SpawnDelegationCommandResult> {
  const response = await createDelegation(parentSessionId, request);
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
  const response = await fetchDelegationStatus(parentSessionId, delegationId);
  return delegationStatusCommandResult(response);
}

export async function getDelegationResultCommand(
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationResultPacket> {
  const response = await fetchDelegationResult(parentSessionId, delegationId);
  return delegationResultPacket(response.result, {
    revision: response.revision,
    serverInstanceId: response.serverInstanceId,
  });
}

export async function cancelDelegationCommand(
  parentSessionId: string,
  delegationId: string,
): Promise<DelegationStatusCommandResult> {
  const response = await cancelDelegation(parentSessionId, delegationId);
  return delegationStatusCommandResult(response);
}

export async function waitDelegationCommand(
  parentSessionId: string,
  delegationId: string,
  options?: WaitDelegationsOptions,
): Promise<WaitDelegationsResult> {
  return waitDelegationsCommand(parentSessionId, [delegationId], options);
}

/**
 * Polls delegation status until every requested delegation is terminal or the
 * timeout expires. Status fetch errors reject the promise; callers that need
 * partial-state reporting should wrap this command and surface their own error
 * outcome. The loop defensively rejects mismatched status ids instead of
 * waiting for an impossible requested id to complete.
 */
export async function waitDelegationsCommand(
  parentSessionId: string,
  delegationIds: readonly string[],
  options: WaitDelegationsOptions = {},
): Promise<WaitDelegationsResult> {
  const ids = normalizeDelegationIds(delegationIds);
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

  if (ids.length === 0) {
    return {
      outcome: "completed",
      delegations: [],
      completed: [],
      pending: [],
      revision: null,
      serverInstanceId: null,
    };
  }

  for (;;) {
    const pendingIds = ids.filter((id) => {
      const record = recordsById.get(id);
      return record === undefined || !delegationStatusIsTerminal(record.status);
    });

    const responses = await Promise.all(
      pendingIds.map((id) => fetchDelegationStatus(parentSessionId, id)),
    );
    for (const [index, response] of responses.entries()) {
      const requestedId = pendingIds[index];
      if (response.delegation.id !== requestedId) {
        throw new MismatchedDelegationIdError(
          requestedId,
          response.delegation.id,
        );
      }
      recordsById.set(requestedId, response.delegation);
      if (
        lastServerInstanceId !== null &&
        response.serverInstanceId !== lastServerInstanceId
      ) {
        throw new MixedDelegationServerInstanceError(
          lastServerInstanceId,
          response.serverInstanceId,
        );
      }
      if (lastRevision === null || response.revision > lastRevision) {
        lastRevision = response.revision;
        lastServerInstanceId = response.serverInstanceId;
      }
    }

    const delegations = ids
      .map((id) => recordsById.get(id))
      .filter((record): record is DelegationRecord => record !== undefined);
    const pending = delegations.filter(
      (record) => !delegationStatusIsTerminal(record.status),
    );
    const completed = delegations.filter((record) =>
      delegationStatusIsTerminal(record.status),
    );
    if (pending.length === 0 && delegations.length === ids.length) {
      return {
        outcome: "completed",
        delegations,
        completed,
        pending,
        revision: lastRevision,
        serverInstanceId: lastServerInstanceId,
      };
    }

    const remainingMs = deadlineAt - Date.now();
    if (remainingMs <= 0) {
      return {
        outcome: "timeout",
        delegations,
        completed,
        pending,
        revision: lastRevision,
        serverInstanceId: lastServerInstanceId,
      };
    }

    await delay(Math.min(pollIntervalMs, remainingMs));
  }
}

export const delegationCommands = {
  spawn_delegation: spawnDelegationCommand,
  get_delegation_status: getDelegationStatusCommand,
  get_delegation_result: getDelegationResultCommand,
  cancel_delegation: cancelDelegationCommand,
  wait_delegation: waitDelegationCommand,
  wait_delegations: waitDelegationsCommand,
} as const;

function delegationStatusCommandResult(response: {
  delegation: DelegationRecord;
  revision: number;
  serverInstanceId: string;
}): DelegationStatusCommandResult {
  return {
    delegationId: response.delegation.id,
    childSessionId: response.delegation.childSessionId,
    status: response.delegation.status,
    delegation: response.delegation,
    revision: response.revision,
    serverInstanceId: response.serverInstanceId,
  };
}

function delegationResultPacket(
  result: DelegationResult,
  metadata: { revision: number; serverInstanceId: string },
): DelegationResultPacket {
  const commandsRun = result.commandsRun ?? [];
  return {
    delegationId: result.delegationId,
    childSessionId: result.childSessionId,
    status: result.status,
    summary: result.summary,
    findings: result.findings ?? [],
    changedFiles: result.changedFiles ?? [],
    commandsRun,
    failedChecks: commandsRun.filter(isFailedDelegationCheck),
    notes: result.notes ?? [],
    revision: metadata.revision,
    serverInstanceId: metadata.serverInstanceId,
  };
}

// Status strings are intentionally allow-listed: only the known successful
// variants below are considered passing, and unknown/empty/warning statuses are
// surfaced in failedChecks so wrappers do not accidentally hide uncertain work.
function isFailedDelegationCheck(command: DelegationCommandResult) {
  const normalized = command.status.trim().toLowerCase();
  return !["ok", "pass", "passed", "success", "successful"].includes(
    normalized,
  );
}

function normalizeDelegationIds(delegationIds: readonly string[]) {
  const seen = new Set<string>();
  const normalizedIds: string[] = [];
  for (const id of delegationIds) {
    const normalized = id.trim();
    if (normalized && !seen.has(normalized)) {
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

function delay(ms: number) {
  return new Promise<void>((resolve) => {
    globalThis.setTimeout(resolve, ms);
  });
}

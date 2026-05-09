// Owns wrapper-facing delegation error packet construction and sanitization for
// wait/status/spawn command failures. It does not own polling, transport
// dispatch, or delegation summary projection; those stay in
// `delegation-commands.ts`. Split out of `ui/src/delegation-commands.ts`
// during the command-surface extraction.

import { ApiRequestError, type ApiRequestErrorKind } from "./api";
import { sanitizeUserFacingErrorMessage } from "./error-messages";

export type MixedServerInstanceErrorPacket = {
  kind: "mixed-server-instance";
  name: string;
  message: string;
  serverInstanceIds: string[];
  recoveryGroups: MixedServerInstanceRecoveryGroup[];
};

export type MixedServerInstanceRecoveryGroup = {
  serverInstanceId: string;
  revision: number;
  delegationIds: string[];
  childSessionIds: string[];
};

export type WaitDelegationErrorPacket =
  | {
      kind: "mismatched-delegation-id";
      name: string;
      message: string;
      requestedId: string;
      receivedId: string;
    }
  | MixedServerInstanceErrorPacket
  | {
      kind: "status-fetch-failed";
      name: string;
      message: string;
      apiErrorKind: ApiRequestErrorKind | null;
      status: number | null;
      restartRequired: boolean | null;
    };

export type WaitDelegationErrorKind = WaitDelegationErrorPacket["kind"];

export type DelegationResumeWaitFailurePacket = {
  kind: "resume-wait-failed";
  name: string;
  message: string;
  apiErrorKind: ApiRequestErrorKind | null;
  status: number | null;
  restartRequired: boolean | null;
};

export type SpawnDelegationFailurePacket =
  | SpawnDelegationTransportFailurePacket
  | SpawnDelegationValidationFailurePacket;

export type SpawnDelegationTransportFailurePacket = {
  kind: "spawn-failed";
  name: string;
  message: string;
  apiErrorKind: ApiRequestErrorKind | null;
  status: number | null;
  restartRequired: boolean | null;
};

export type SpawnDelegationValidationFailurePacket = {
  kind: "validation-failed";
  name: string;
  message: string;
};

export type SpawnReviewerBatchErrorPacket =
  | MixedServerInstanceErrorPacket
  | {
      kind: "all-spawns-failed";
      name: string;
      message: string;
    }
  | SpawnDelegationValidationFailurePacket;

const GENERIC_DELEGATION_STATUS_FETCH_ERROR_MESSAGE =
  "Delegation status fetch failed.";
const GENERIC_DELEGATION_RESUME_WAIT_ERROR_MESSAGE =
  "Delegation resume wait scheduling failed.";
const GENERIC_SPAWN_DELEGATION_ERROR_MESSAGE = "Spawn delegation failed.";
const GENERIC_SPAWN_DELEGATION_VALIDATION_ERROR_MESSAGE =
  "Invalid delegation request.";
const SAFE_SPAWN_DELEGATION_VALIDATION_NAMES = new Set([
  "TypeError",
  "RangeError",
  "Error",
]);
// Audit boundary: every message allowed here is forwarded to wrapper callers.
// New entries require reviewing every ApiRequestError constructor that can emit
// that message family.
const SAFE_DELEGATION_STATUS_FETCH_MESSAGES = new Set([
  "The TermAl backend is unavailable.",
]);
const SAFE_DELEGATION_RESUME_WAIT_MESSAGES = new Set([
  "The TermAl backend is unavailable.",
  "session not found",
  "delegation not found",
  "delegation wait ids cannot be empty",
  "delegation wait requires at least one delegation id",
]);
const SAFE_DELEGATION_STATUS_FETCH_STATUS_PATTERN =
  /^Request failed with status \d+\.$/u;
const SAFE_DELEGATION_RESUME_WAIT_PATTERNS = [
  /^Request failed with status \d+\.$/u,
  /^delegation wait accepts at most \d+ delegation ids$/u,
  /^delegation wait title must be at most \d+ characters$/u,
  /^delegation `[^`]+` does not belong to parent session `[^`]+`$/u,
];
const SAFE_SPAWN_DELEGATION_MESSAGES = new Set([
  "The TermAl backend is unavailable.",
  "parent session id is required",
  "session not found",
  "delegation prompt cannot be empty",
  "worker delegations are not implemented in Phase 1",
  "only readOnly delegation write policy is implemented in Phase 1",
  "delegations for remote-backed sessions are not implemented in Phase 1",
  "delegations for remote-backed projects are not implemented in Phase 1",
]);
const SAFE_SPAWN_DELEGATION_PATTERNS = [
  /^Request failed with status \d+\.$/u,
  /^delegation prompt must be at most \d+ bytes$/u,
  /^delegation title must be at most \d+ characters$/u,
  /^delegation model must be at most \d+ characters$/u,
  /^parent session already has \d+ active delegations$/u,
  /^delegation nesting depth is limited to \d+$/u,
  /^delegation cwd cannot be (?:a drive-relative Windows path|a UNC path|a Windows device namespace path)$/u,
  /^unknown project `[^`]+`$/u,
  /^delegation cwd `[^`]+` must stay inside project `[^`]+`$/u,
];
const SAFE_SPAWN_DELEGATION_VALIDATION_MESSAGES = new Set([
  "parent session id must be a string",
  "parent session id must be non-empty",
  "parent session id must not contain /, ?, #, or control characters",
  "prompt must be a string",
  "prompt must be non-empty",
  "title must be omitted instead of null",
  "cwd must be omitted instead of null",
  "agent must be omitted instead of null",
  "model must be omitted instead of null",
  // Direct `spawn_delegation` callers may pass these optional fields as null.
  // `spawn_reviewer_batch` overwrites them before validation.
  "mode must be omitted instead of null",
  "writePolicy must be omitted instead of null",
  "spawn_reviewer_batch requests must be an array",
  "spawn_reviewer_batch requires at least one reviewer",
]);
const SAFE_SPAWN_DELEGATION_VALIDATION_PATTERNS = [
  /^prompt must be no larger than \d+ bytes$/u,
  /^title must be no longer than \d+ characters$/u,
  /^model must be no longer than \d+ characters$/u,
  /^spawn_reviewer_batch accepts at most \d+ reviewers$/u,
  /^reviewer request \d+ must be an object$/u,
];
// Paired with `formatUnavailableApiMessage` in `ui/src/api.ts` for the
// delegation status route. Keep this suffix in sync with that helper's wording.
const SAFE_DELEGATION_STATUS_FETCH_UNAVAILABLE_ROUTE_SUFFIX_PATTERN =
  /^(?: \(HTTP \d+\))?\. Restart TermAl so the latest API routes are loaded\.$/u;

type StatusFetchErrorContext = {
  parentSessionId: string;
  delegationId: string;
};

type SpawnDelegationErrorContext = {
  parentSessionId: string;
};

type DelegationResumeWaitErrorContext = {
  parentSessionId: string;
};

export function statusFetchErrorPacket(
  error: unknown,
  context: StatusFetchErrorContext,
): WaitDelegationErrorPacket {
  if (error instanceof ApiRequestError) {
    return {
      kind: "status-fetch-failed",
      name: error.name,
      message: safeDelegationStatusFetchMessage(
        error.message,
        context,
        error.kind,
      ),
      apiErrorKind: error.kind,
      status: error.status,
      restartRequired: error.restartRequired,
    };
  }
  const name = error instanceof Error ? error.name : "Error";
  return {
    kind: "status-fetch-failed",
    name,
    message: GENERIC_DELEGATION_STATUS_FETCH_ERROR_MESSAGE,
    apiErrorKind: null,
    status: null,
    restartRequired: null,
  };
}

export function resumeWaitFailurePacket(
  error: unknown,
  context: DelegationResumeWaitErrorContext,
): DelegationResumeWaitFailurePacket {
  if (error instanceof ApiRequestError) {
    return {
      kind: "resume-wait-failed",
      name: error.name,
      message: safeDelegationResumeWaitMessage(
        error.message,
        context,
        error.kind,
      ),
      apiErrorKind: error.kind,
      status: error.status,
      restartRequired: error.restartRequired,
    };
  }
  const name = error instanceof Error ? error.name : "Error";
  return {
    kind: "resume-wait-failed",
    name,
    message: GENERIC_DELEGATION_RESUME_WAIT_ERROR_MESSAGE,
    apiErrorKind: null,
    status: null,
    restartRequired: null,
  };
}

export function spawnDelegationFailurePacket(
  error: unknown,
  context: SpawnDelegationErrorContext,
): SpawnDelegationTransportFailurePacket {
  if (error instanceof ApiRequestError) {
    return {
      kind: "spawn-failed",
      name: error.name,
      message: safeSpawnDelegationFailureMessage(
        error.message,
        context,
        error.kind,
      ),
      apiErrorKind: error.kind,
      status: error.status,
      restartRequired: error.restartRequired,
    };
  }
  const name = error instanceof Error ? error.name : "Error";
  return {
    kind: "spawn-failed",
    name,
    message: GENERIC_SPAWN_DELEGATION_ERROR_MESSAGE,
    apiErrorKind: null,
    status: null,
    restartRequired: null,
  };
}

export function spawnDelegationValidationFailurePacket(
  error: unknown,
): SpawnDelegationValidationFailurePacket {
  return {
    kind: "validation-failed",
    name: safeSpawnDelegationValidationFailureName(error),
    message: safeSpawnDelegationValidationFailureMessage(error),
  };
}

export function mismatchedDelegationIdErrorPacket(
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

export function mixedServerInstanceErrorPacket(
  serverInstanceIds: readonly string[],
  options: {
    operation?: "status-batch" | "spawn-batch";
    recoveryGroups?: readonly MixedServerInstanceRecoveryGroup[];
  } = {},
): MixedServerInstanceErrorPacket {
  const uniqueServerInstanceIds = [...new Set(serverInstanceIds)].sort();
  const operation =
    options.operation === "spawn-batch"
      ? "delegation spawn batch"
      : "delegation status batch";
  return {
    kind: "mixed-server-instance",
    name: "MixedDelegationServerInstanceError",
    message: `${operation} contained multiple server instances: ${uniqueServerInstanceIds.join(", ")}`,
    serverInstanceIds: uniqueServerInstanceIds,
    recoveryGroups: [...(options.recoveryGroups ?? [])].map((group) => ({
      serverInstanceId: group.serverInstanceId,
      revision: group.revision,
      delegationIds: [...group.delegationIds],
      childSessionIds: [...group.childSessionIds],
    })),
  };
}

function safeDelegationStatusFetchMessage(
  message: string,
  context: StatusFetchErrorContext,
  apiErrorKind?: ApiRequestErrorKind,
) {
  const sanitized = sanitizeUserFacingErrorMessage(message);
  if (
    SAFE_DELEGATION_STATUS_FETCH_MESSAGES.has(sanitized) ||
    SAFE_DELEGATION_STATUS_FETCH_STATUS_PATTERN.test(sanitized) ||
    (apiErrorKind === "backend-unavailable" &&
      isSafeUnavailableRouteDiagnostic(sanitized, statusFetchEndpoint(context)))
  ) {
    return sanitized;
  }
  return GENERIC_DELEGATION_STATUS_FETCH_ERROR_MESSAGE;
}

function safeDelegationResumeWaitMessage(
  message: string,
  context: DelegationResumeWaitErrorContext,
  apiErrorKind?: ApiRequestErrorKind,
) {
  const sanitized = sanitizeUserFacingErrorMessage(message);
  if (
    SAFE_DELEGATION_RESUME_WAIT_MESSAGES.has(sanitized) ||
    SAFE_DELEGATION_RESUME_WAIT_PATTERNS.some((pattern) =>
      pattern.test(sanitized),
    ) ||
    (apiErrorKind === "backend-unavailable" &&
      isSafeUnavailableRouteDiagnostic(
        sanitized,
        `/api/sessions/${encodeURIComponent(context.parentSessionId)}/delegation-waits`,
      ))
  ) {
    return sanitized;
  }
  return GENERIC_DELEGATION_RESUME_WAIT_ERROR_MESSAGE;
}

function safeSpawnDelegationFailureMessage(
  message: string,
  context: SpawnDelegationErrorContext,
  apiErrorKind?: ApiRequestErrorKind,
) {
  const sanitized = sanitizeUserFacingErrorMessage(message);
  if (
    SAFE_SPAWN_DELEGATION_MESSAGES.has(sanitized) ||
    SAFE_SPAWN_DELEGATION_PATTERNS.some((pattern) => pattern.test(sanitized)) ||
    (apiErrorKind === "backend-unavailable" &&
      isSafeUnavailableRouteDiagnostic(
        sanitized,
        `/api/sessions/${encodeURIComponent(context.parentSessionId)}/delegations`,
      ))
  ) {
    return sanitized;
  }
  return GENERIC_SPAWN_DELEGATION_ERROR_MESSAGE;
}

function safeSpawnDelegationValidationFailureMessage(error: unknown) {
  if (error instanceof TypeError || error instanceof RangeError) {
    const sanitized = sanitizeUserFacingErrorMessage(error.message);
    if (
      SAFE_SPAWN_DELEGATION_VALIDATION_MESSAGES.has(sanitized) ||
      SAFE_SPAWN_DELEGATION_VALIDATION_PATTERNS.some((pattern) =>
        pattern.test(sanitized),
      )
    ) {
      return sanitized;
    }
  }
  return GENERIC_SPAWN_DELEGATION_VALIDATION_ERROR_MESSAGE;
}

function safeSpawnDelegationValidationFailureName(error: unknown) {
  if (
    error instanceof Error &&
    SAFE_SPAWN_DELEGATION_VALIDATION_NAMES.has(error.name)
  ) {
    return error.name;
  }
  return "Error";
}

function isSafeUnavailableRouteDiagnostic(
  message: string,
  expectedEndpoint: string,
) {
  const prefix = `The running backend does not expose ${expectedEndpoint}`;
  return (
    message.startsWith(prefix) &&
    SAFE_DELEGATION_STATUS_FETCH_UNAVAILABLE_ROUTE_SUFFIX_PATTERN.test(
      message.slice(prefix.length),
    )
  );
}

function statusFetchEndpoint({
  parentSessionId,
  delegationId,
}: StatusFetchErrorContext) {
  return `/api/sessions/${encodeURIComponent(parentSessionId)}/delegations/${encodeURIComponent(delegationId)}`;
}

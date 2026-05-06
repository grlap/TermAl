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

export type SpawnDelegationFailurePacket = {
  name: string;
  message: string;
  apiErrorKind: ApiRequestErrorKind | null;
  status: number | null;
  restartRequired: boolean | null;
};

const GENERIC_DELEGATION_STATUS_FETCH_ERROR_MESSAGE =
  "Delegation status fetch failed.";
const GENERIC_SPAWN_DELEGATION_ERROR_MESSAGE = "Spawn delegation failed.";
// Audit boundary: every message allowed here is forwarded to wrapper callers.
// New entries require reviewing every ApiRequestError constructor that can emit
// that message family.
const SAFE_DELEGATION_STATUS_FETCH_MESSAGES = new Set([
  "The TermAl backend is unavailable.",
]);
const SAFE_DELEGATION_STATUS_FETCH_STATUS_PATTERN =
  /^Request failed with status \d+\.$/u;
const SAFE_SPAWN_DELEGATION_MESSAGES = new Set([
  "The TermAl backend is unavailable.",
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

export function spawnDelegationFailurePacket(
  error: unknown,
  context: SpawnDelegationErrorContext,
): SpawnDelegationFailurePacket {
  if (error instanceof ApiRequestError) {
    return {
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
    name,
    message: GENERIC_SPAWN_DELEGATION_ERROR_MESSAGE,
    apiErrorKind: null,
    status: null,
    restartRequired: null,
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
): MixedServerInstanceErrorPacket {
  const uniqueServerInstanceIds = [...new Set(serverInstanceIds)].sort();
  return {
    kind: "mixed-server-instance",
    name: "MixedDelegationServerInstanceError",
    message: `delegation status batch contained multiple server instances: ${uniqueServerInstanceIds.join(", ")}`,
    serverInstanceIds: uniqueServerInstanceIds,
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

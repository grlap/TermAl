// Owns wrapper-facing delegation error packet construction and sanitization for
// wait/status command failures. It does not own polling, transport dispatch, or
// delegation summary projection; those stay in `delegation-commands.ts`.

import { ApiRequestError, type ApiRequestErrorKind } from "./api";
import { sanitizeUserFacingErrorMessage } from "./error-messages";

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
      apiErrorKind: ApiRequestErrorKind | null;
      status: number | null;
      restartRequired: boolean | null;
    };

export type WaitDelegationErrorKind = WaitDelegationErrorPacket["kind"];

const GENERIC_DELEGATION_STATUS_FETCH_ERROR_MESSAGE =
  "Delegation status fetch failed.";
// Audit boundary: every message allowed here is forwarded to wrapper callers.
// New entries require reviewing every ApiRequestError constructor that can emit
// that message family.
const SAFE_DELEGATION_STATUS_FETCH_MESSAGES = new Set([
  "The TermAl backend is unavailable.",
]);
const SAFE_DELEGATION_STATUS_FETCH_STATUS_PATTERN =
  /^Request failed with status \d+\.$/u;
const SAFE_DELEGATION_STATUS_FETCH_UNAVAILABLE_ROUTE_PATTERN =
  /^The running backend does not expose \/api\/sessions\/[^/?#\s]+\/delegations\/[^/?#\s]+(?: \(HTTP \d+\))?\. Restart TermAl so the latest API routes are loaded\.$/u;

export function statusFetchErrorPacket(
  error: unknown,
): WaitDelegationErrorPacket {
  if (error instanceof ApiRequestError) {
    return {
      kind: "status-fetch-failed",
      name: error.name,
      message: safeDelegationStatusFetchMessage(error.message, error.kind),
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
): WaitDelegationErrorPacket {
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
  apiErrorKind?: ApiRequestErrorKind,
) {
  const sanitized = sanitizeUserFacingErrorMessage(message);
  if (
    SAFE_DELEGATION_STATUS_FETCH_MESSAGES.has(sanitized) ||
    SAFE_DELEGATION_STATUS_FETCH_STATUS_PATTERN.test(sanitized) ||
    (apiErrorKind === "backend-unavailable" &&
      SAFE_DELEGATION_STATUS_FETCH_UNAVAILABLE_ROUTE_PATTERN.test(sanitized))
  ) {
    return sanitized;
  }
  return GENERIC_DELEGATION_STATUS_FETCH_ERROR_MESSAGE;
}

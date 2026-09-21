// Owns tab-local recovery of an exact operator policy request across dialog remounts/reloads.
// No network writes; storage must succeed before the first send is allowed.
import type { PolicyChange } from "./acceptance-settings-api";

const key = (project: string) => `termal.acceptance-policy.pending.v1:${project}`;
export function readPolicyAttempt(project: string): { attempt: PolicyChange | null; error: string | null } {
  let raw: string | null;
  try { raw = sessionStorage.getItem(key(project)); }
  catch {
    return { attempt: null, error: "Policy recovery storage is unavailable. Restore browser session storage and reopen this editor before sending a policy change." };
  }
  try {
    if (!raw) return { attempt: null, error: null };
    const value = JSON.parse(raw) as PolicyChange;
    if (!value || Object.keys(value).some(field => !["modes", "mechanicalBasis", "requireSourceFreshness", "readerKey", "expectedPolicy", "idempotencyKey"].includes(field))
      || !Array.isArray(value.modes) || !value.modes.every(mode => ["same_session", "sub_agent", "independent_session"].includes(mode))
      || !["asserted", "observed"].includes(value.mechanicalBasis) || typeof value.requireSourceFreshness !== "boolean"
      || ![value.readerKey, value.expectedPolicy, value.idempotencyKey].every(v => typeof v === "string" && v.length > 0)) {
      throw new Error("Invalid saved policy attempt");
    }
    return { attempt: value, error: null };
  } catch {
    return { attempt: null, error: "The saved policy attempt is unreadable. No policy change can be sent. Reconcile the store's policy before discarding recovery data; closing this browser tab clears its saved attempt." };
  }
}

export function storePolicyAttempt(project: string, attempt: PolicyChange | null) {
  if (attempt) sessionStorage.setItem(key(project), JSON.stringify(attempt));
  else sessionStorage.removeItem(key(project));
}

export function clearPolicyAttempt(project: string, resolved: PolicyChange) {
  const current = readPolicyAttempt(project);
  if (current.error) throw new Error(current.error);
  // A late response from a closed dialog must not erase a newer attempt saved
  // by its replacement. Storage operations here are synchronous within the tab.
  if (current.attempt?.idempotencyKey === resolved.idempotencyKey) {
    sessionStorage.removeItem(key(project));
  }
}

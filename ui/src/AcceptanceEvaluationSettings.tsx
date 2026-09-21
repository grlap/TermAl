// Owns evaluator defaults and explicit store-policy editing in project settings.
// Does not enable Engram, run doctor, or submit task verdicts. New focused editor.
import { useEffect, useRef, useState } from "react";
import { ApiRequestError } from "./api-request";
import { changeAcceptancePolicy, getAcceptancePolicy, saveEvaluatorDefaults,
  type AcceptancePolicySnapshot, type EvaluatorDefaults, type PolicyChange, type StoreAcceptancePolicy } from "./acceptance-settings-api";
import type { StateResponse } from "./api";
import type { AcceptanceEvaluationMode } from "./types";
import "./acceptance-evaluation-settings.css";
import { clearPolicyAttempt, readPolicyAttempt, storePolicyAttempt } from "./acceptance-policy-recovery";
import { useCommittedRef } from "./panels/use-committed-ref";

const modes: [AcceptanceEvaluationMode, string][] = [
  ["same_session", "Same session"], ["sub_agent", "Sub-agent"], ["independent_session", "Independent session"],
];
const describe = (value: unknown) => value instanceof Error ? value.message : String(value);

export type AcceptanceEvaluationSettingsProps = {
  projectId: string; enabled: boolean; value: EvaluatorDefaults; onChange: (value: EvaluatorDefaults) => void;
  busy: boolean; onBusyChange: (busy: boolean) => void; onSaved: (state: StateResponse) => void;
};

export function AcceptanceEvaluationSettings(props: AcceptanceEvaluationSettingsProps) {
  // This boundary owns per-project recovery-state initialization.
  return <AcceptanceEvaluationSettingsBody key={props.projectId} {...props} />;
}

function AcceptanceEvaluationSettingsBody({ projectId, enabled, value, onChange, busy, onBusyChange, onSaved }: AcceptanceEvaluationSettingsProps) {
  const [recovery] = useState(() => readPolicyAttempt(projectId));
  const [recoveryBlocked, setRecoveryBlocked] = useState(!!recovery.error);
  const mounted = useRef(true);
  const busyCallback = useCommittedRef(onBusyChange);
  const policyRead = useRef<AbortController | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      // Release this mounted form's busy ownership immediately. Its late
      // completions remain fenced and cannot unlock a replacement form's save.
      busyCallback.current(false);
    };
  }, [busyCallback]);
  const [policy, setPolicy] = useState<AcceptancePolicySnapshot | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(recovery.error);
  const [refresh, setRefresh] = useState(0);
  const [editing, setEditing] = useState(!!recovery.attempt);
  const [draft, setDraft] = useState<StoreAcceptancePolicy>(recovery.attempt ?? { modes: [], mechanicalBasis: "asserted", requireSourceFreshness: false });
  const [base, setBase] = useState<{ policy: string; readerKey: string } | null>(null);
  const [confirmed, setConfirmed] = useState(!!recovery.attempt);
  const [attempt, setAttempt] = useState<PolicyChange | null>(recovery.attempt);
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    const abort = new AbortController();
    policyRead.current = abort;
    setPolicy(null); setReadError(null);
    if (enabled) void getAcceptancePolicy(projectId, abort.signal).then(result => {
      if (!abort.signal.aborted) setPolicy(result);
    }).catch(failure => { if (!abort.signal.aborted) setReadError(describe(failure)); });
    return () => abort.abort();
  }, [projectId, enabled, refresh]);
  const store = policy?.acceptanceEvaluation;
  function clearRecovery(resolved: PolicyChange) {
    try { clearPolicyAttempt(projectId, resolved); return true; }
    catch {
      if (mounted.current) {
        setRecoveryBlocked(true);
        setError("The response arrived, but recovery storage could not be cleared. Restore storage and reopen this editor before another change.");
      }
      return false;
    }
  }
  async function saveDefaults() {
    const normalized = { ...value, evaluatorModel: value.evaluatorModel?.trim() || undefined };
    if (normalized.evaluatorModel && new TextEncoder().encode(normalized.evaluatorModel).length > 128) {
      setError("Evaluator model must be at most 128 UTF-8 bytes."); return;
    }
    setSaving(true); onBusyChange(true); setError(null);
    try { const result = await saveEvaluatorDefaults(projectId, normalized); if (mounted.current) onSaved(result); }
    catch (failure) { if (mounted.current) setError(describe(failure)); }
    finally { if (mounted.current) { setSaving(false); onBusyChange(false); } }
  }
  async function savePolicy() {
    if (recoveryBlocked || (!attempt && (!base || !confirmed))) return;
    const change = attempt ?? { ...draft, expectedPolicy: base!.policy, readerKey: base!.readerKey,
      idempotencyKey: `termal-policy-${crypto.randomUUID()}` };
    try { storePolicyAttempt(projectId, change); }
    catch { setError("Could not save recovery state. No policy change was sent."); return; }
    // An older GET must not overwrite a newer write result, even if its transport
    // ignores cancellation and resolves later.
    policyRead.current?.abort();
    setAttempt(change); setSaving(true); onBusyChange(true); setError(null);
    try {
      const result = await changeAcceptancePolicy(projectId, change);
      const cleared = clearRecovery(change);
      if (!mounted.current) return;
      setPolicy(result);
      setReadError(null);
      if (!cleared) setError("Policy change applied, but recovery storage could not be cleared. Restore storage and reopen this editor before another change.");
      else if (result.writeApplied && result.error) setError(`Policy change applied. ${result.error}`);
      setAttempt(null); setEditing(false); setConfirmed(false);
    } catch (failure) {
      if (mounted.current) setError(describe(failure));
      if (!attempt && failure instanceof ApiRequestError && failure.status === 409) {
        // A first-send refusal is correctable. On recovery, a reset/reader/CAS
        // refusal does not prove what happened to the earlier send: keep its key.
        if (!clearRecovery(change)) return;
        if (!mounted.current) return;
        setAttempt(null); setEditing(false); setRefresh(n => n + 1);
      } else if (!attempt && failure instanceof ApiRequestError
        && [400, 403, 404, 422, 429].includes(failure.status ?? 0)) {
        // These admission/validation refusals sent nothing on a first try.
        // They cannot resolve an earlier uncertain attempt, however.
        if (!clearRecovery(change)) return;
        if (mounted.current) setAttempt(null);
      }
    } finally { if (mounted.current) { setSaving(false); onBusyChange(false); } }
  }
  return <section className="acceptance-evaluation-settings" aria-label="Acceptance evaluation">
    <h3>Acceptance evaluation</h3>
    <p className="settings-panel-copy" role="status">
      {!enabled ? "Enable Engram to read its store policy." : readError ? `Policy unknown — ${readError}` : !policy ? "Reading policy…"
        : !policy.available || !store ? `Policy unknown — ${policy.error ?? "older Engram binary"}`
          : !store.modes.length ? "Off — completion is self-asserted."
            : `Required · ${store.modes.map(mode => modes.find(([key]) => key === mode)?.[1] ?? mode).join(", ")} · basis ${store.mechanicalBasis} · source freshness ${store.requireSourceFreshness ? "required" : "not required"}`}
    </p>
    <fieldset disabled={busy || saving || !!attempt}>
      <label>Default evaluator mode
        <select aria-label="Default evaluator mode" value={value.defaultMode ?? ""}
          onChange={e => { const mode = modes.find(([mode]) => mode === e.target.value)?.[0]; if (mode !== "sub_agent") onChange({ ...value, defaultMode: mode }); }}>
          <option value="">Auto — strongest admitted mode</option>
          {modes.map(([mode, name]) => <option key={mode} value={mode}
            disabled={mode === "sub_agent" || (!!store && !store.modes.includes(mode))}>
            {name}{mode === "sub_agent" ? " — not produced by this host yet" : store && !store.modes.includes(mode) ? " — not admitted by policy" : ""}
          </option>)}
        </select>
      </label>
      <label>Evaluator agent
        <select aria-label="Evaluator agent" value={value.evaluatorAgent ?? ""}
          onChange={e => { const agent = e.target.value; if (agent === "" || agent === "Claude" || agent === "Codex") onChange({ ...value, evaluatorAgent: agent || undefined, evaluatorModel: undefined }); }}>
          <option value="">Auto — other vendor when ready</option><option>Claude</option><option>Codex</option>
        </select>
      </label>
      <label>Evaluator model override
        <input aria-label="Evaluator model override" disabled={!value.evaluatorAgent} value={value.evaluatorModel ?? ""} maxLength={128}
          onChange={e => onChange({ ...value, evaluatorModel: e.target.value || undefined })} placeholder="Agent default" />
      </label>
      <p className="create-session-field-hint">Choose a specific evaluator agent to save a model override. Changing the agent clears its model override.</p>
      <p className="create-session-field-hint">Task pins take precedence. Defaults change only future evaluations, without auditing the store or resetting sessions.</p>
      <button type="button" className="ghost-button" disabled={!enabled} onClick={() => void saveDefaults()}>Save evaluator defaults</button>
      <button type="button" className="ghost-button" disabled={recoveryBlocked || !policy?.available || !store || !policy.policy}
        onClick={() => { if (store && policy?.policy) { setDraft({ ...store, modes: [...store.modes] }); setBase({ policy: policy.policy, readerKey: policy.readerKey }); } setEditing(true); setConfirmed(false); setError(null); }}>
        Change store policy…
      </button>
      <button type="button" className="ghost-button" disabled={!enabled || !!attempt} onClick={() => setRefresh(n => n + 1)}>Refresh policy</button>
    </fieldset>
    {editing && <div role="group" aria-label="Change store acceptance policy">
      <p>This changes the completion rules for every session on this Engram store. No modes means completion is self-asserted.</p>
      <fieldset disabled={busy || saving || !!attempt}>
        {modes.map(([mode, name]) => <label key={mode}><input type="checkbox" checked={draft.modes.includes(mode)}
          onChange={e => setDraft({ ...draft, modes: e.target.checked ? [...draft.modes, mode] : draft.modes.filter(m => m !== mode) })} />Allow {name}</label>)}
        <label>Mechanical evidence basis<select aria-label="Mechanical evidence basis" value={draft.mechanicalBasis}
          onChange={e => { const basis = e.target.value; if (basis === "asserted" || basis === "observed") setDraft({ ...draft, mechanicalBasis: basis }); }}>
          <option value="asserted">Asserted</option><option value="observed">Observed</option>
        </select></label>
        <label><input type="checkbox" checked={draft.requireSourceFreshness} onChange={e => setDraft({ ...draft, requireSourceFreshness: e.target.checked })} />Require source freshness</label>
        <p>Sub-agent evaluations, observed build evidence and source fingerprints are not yet produced by this host. Requiring them can block completion until another capable host supplies them.</p>
        <label><input type="checkbox" checked={confirmed} onChange={e => setConfirmed(e.target.checked)} />I confirm this store-wide policy change</label>
      </fieldset>
      <button type="button" className="primary-button" disabled={busy || saving || !enabled || recoveryBlocked || !confirmed} onClick={() => void savePolicy()}>
        {saving ? "Saving policy…" : attempt ? "Retry identical policy change" : "Confirm policy change"}
      </button>
      {!attempt && <button type="button" className="ghost-button" disabled={busy || saving} onClick={() => setEditing(false)}>Cancel policy change</button>}
      {attempt && !saving && <p role="status">The write may have succeeded. Retry uses the same change and idempotency key; do not make a different change until its outcome is resolved.</p>}
      {attempt && !enabled && <p>Re-enable Engram for the original store to retry this saved change. Disabling the integration does not resolve its earlier write outcome.</p>}
    </div>}
    {error && <p role="alert" className="inline-error">{error}</p>}
  </section>;
}

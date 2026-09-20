// Explicit audit action/result alongside the connection editor. New boundary
// extracted from the diagnostics responsibility of EngramProjectSettingsPanel;
// does not verify, save settings, or authorize readiness from an audit result.
import { useEffect, useMemo, useRef, useState } from "react";
import { request } from "./api-request";
import type { EngramFullAudit } from "./types";

// Defensive UI ceiling even when an older/misconfigured server exceeds its
// byte budget. Avoid splitting a UTF-16 surrogate pair at the preview boundary.
function auditTextPreview(text: string, limit: number): string {
  if (text.length <= limit) return text;
  let end = limit;
  if (text.charCodeAt(end - 1) >= 0xd800 && text.charCodeAt(end - 1) <= 0xdbff) end--;
  return `${text.slice(0, end)}\n[truncated]`;
}

export function EngramFullAuditPanel({ projectId }: { projectId: string }) {
  const [result, setResult] = useState<EngramFullAudit | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [seconds, setSeconds] = useState(0);
  const [reportOpen, setReportOpen] = useState(false);
  const preview = useMemo(() => result && ({
    report: auditTextPreview(result.reportPreview, 16 * 1024),
    warnings: auditTextPreview(result.warnings, 4096),
  }), [result]);
  const active = useRef<AbortController | null>(null);
  useEffect(() => () => active.current?.abort(), []);
  useEffect(() => {
    if (!running) return;
    const started = Date.now();
    const timer = window.setInterval(() => setSeconds(Math.floor((Date.now() - started) / 1000)), 1000);
    return () => window.clearInterval(timer);
  }, [running]);

  async function audit() {
    if (active.current) return;
    const controller = new AbortController();
    active.current = controller;
    setRunning(true);
    setSeconds(0);
    setError(null);
    try {
      const next = await request<EngramFullAudit>(
        `/api/projects/${encodeURIComponent(projectId)}/engram/audit`,
        { method: "POST", signal: controller.signal,
          headers: { "X-TermAl-Operator-Action": "engram-full-audit" } },
      );
      if (!controller.signal.aborted) setResult(next);
    } catch (failure) {
      if (!controller.signal.aborted) setError(auditTextPreview(failure instanceof Error ? failure.message : String(failure), 4096));
    } finally {
      if (!controller.signal.aborted) setRunning(false);
      active.current = null;
    }
  }

  return <section aria-label="Engram Full Audit" className="project-engram-verification">
    <h4>Full Audit</h4>
    <p className="create-session-field-hint">
      Explicit whole-store doctor audit, separate from Verify and Save. May take several minutes.
      Doctor opens the store writable and may perform SQLite recovery; it does not change TermAl settings.
    </p>
    <button type="button" className="ghost-button" disabled={running} onClick={() => void audit()}>
      {running ? "Auditing…" : "Run Full Audit"}
    </button>
    {running && <p role="status">Full Audit running · {seconds}s elapsed (five-minute limit).</p>}
    {error && <p role="alert">Full Audit failed: {error}</p>}
    {result && <div aria-label="Last Full Audit result">
      <p>{result.healthy ? "Audit healthy" : "Audit unhealthy"} · {result.checkedAt} · {result.elapsedMs}ms</p>
      <p>{result.projectId} · {result.database}</p>
      {preview?.warnings && <pre aria-label="Audit warnings">{preview.warnings}</pre>}
      <details onToggle={event => setReportOpen(event.currentTarget.open)}>
        <summary>Audit report</summary>
        {reportOpen && <>
          {result.reportTruncated && <p>Report truncated to a bounded preview; this is not the complete JSON receipt.</p>}
          <pre aria-label="Audit report preview">{preview?.report}</pre>
        </>}
      </details>
    </div>}
    <p className="create-session-field-hint">
      Readiness is not a full health check. The development redactor provides no secret/PII protection;
      action gating and organizational-authority mediation are not supported.
    </p>
  </section>;
}

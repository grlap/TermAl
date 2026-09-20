// Explicit-audit lifecycle and separation from readiness in the real settings
// panel; fixtures use mocked HTTP only, never a live Engram store.
import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { EngramFullAuditPanel } from "./EngramFullAuditPanel";
import { EngramProjectSettingsPanel } from "./EngramProjectSettingsPanel";
import { request } from "./api-request";
import { verifyProjectEngramSettings } from "./api";
import type { EngramFullAudit, Project } from "./types";

vi.mock("./api-request", () => ({ request: vi.fn() }));
vi.mock("./api", () => ({ verifyProjectEngramSettings: vi.fn(), updateProjectEngramSettings: vi.fn() }));
vi.mock("./AcceptanceEvaluationSettings", () => ({ AcceptanceEvaluationSettings: () => null }));
const audit: EngramFullAudit = { healthy: true, projectId: "store-one", database: "/home/one.db",
  checkedAt: "2026-09-20T01:00:00Z", elapsedMs: 70, warnings: "No-op redactor provides no protection",
  reportPreview: '{"healthy":true}', reportTruncated: false };

beforeEach(() => vi.resetAllMocks());

it("runs only after explicit action and retains an audit across Verify", async () => {
  vi.mocked(request).mockResolvedValue(audit);
  vi.mocked(verifyProjectEngramSettings).mockResolvedValue({ verified: true, ready: true, fullAudit: "not_run",
    hostPathStatus: "matched", elapsedMs: 10, binaryPath: "/engram", home: "/home", projectId: "store-one",
    database: "/home/one.db", requiredAssurance: "turn_gated" });
  const project: Project = { id: "project/one", name: "One", rootPath: "/one", remoteId: "local", engramDeclared: true };
  render(<EngramProjectSettingsPanel project={project} idPrefix="test" onSaved={vi.fn()} onDefaultsSaved={vi.fn()} onVerified={vi.fn()} />);
  expect(request).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Run Full Audit" }));
  expect(await screen.findByText(/Audit healthy/)).toBeInTheDocument();
  expect(request).toHaveBeenCalledWith("/api/projects/project%2Fone/engram/audit", expect.objectContaining({ method: "POST",
    headers: { "X-TermAl-Operator-Action": "engram-full-audit" } }));
  fireEvent.click(screen.getByRole("button", { name: "Verify" }));
  expect(await screen.findByText("Not run by Verify")).toBeInTheDocument();
  expect(screen.getByLabelText("Last Full Audit result")).toHaveTextContent("Audit healthy");
  expect(screen.getByText(audit.warnings)).toBeInTheDocument();
  expect(screen.getByText(/Base · advisory \/ unmediated/)).toBeInTheDocument();
  expect(request).toHaveBeenCalledTimes(1);
});

it("shows progress, retains prior evidence on failure, and permits explicit retry", async () => {
  vi.mocked(request).mockResolvedValueOnce(audit);
  render(<EngramFullAuditPanel projectId="one" />);
  fireEvent.click(screen.getByRole("button", { name: "Run Full Audit" }));
  await screen.findByText(/Audit healthy/);
  let reject!: (error: Error) => void;
  vi.mocked(request).mockImplementationOnce(() => new Promise((_, fail) => { reject = fail; }));
  fireEvent.click(screen.getByRole("button", { name: "Run Full Audit" }));
  expect(screen.getByRole("button", { name: "Auditing…" })).toBeDisabled();
  expect(screen.getByRole("status")).toHaveTextContent("Full Audit running");
  await act(async () => reject(new Error("deadline exceeded")));
  expect(screen.getByRole("alert")).toHaveTextContent("deadline exceeded");
  expect(screen.getByLabelText("Last Full Audit result")).toHaveTextContent("Audit healthy");
  expect(screen.getByRole("button", { name: "Run Full Audit" })).toBeEnabled();
});

it("aborts observation on unmount without publishing a late result", async () => {
  let resolve!: (result: EngramFullAudit) => void;
  vi.mocked(request).mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  const view = render(<EngramFullAuditPanel projectId="one" />);
  fireEvent.click(screen.getByRole("button", { name: "Run Full Audit" }));
  const signal = vi.mocked(request).mock.calls[0][1]?.signal;
  view.unmount();
  expect(signal?.aborted).toBe(true);
  await act(async () => resolve(audit));
  expect(screen.queryByLabelText("Last Full Audit result")).toBeNull();
});

it("renders bounded previews only when expanded and does not reprocess them on timer ticks", async () => {
  const readPreview = vi.fn(() => `${"x".repeat(16 * 1024 - 1)}💡${"x".repeat(100000)}`);
  const largeAudit: EngramFullAudit = {
    ...audit,
    get reportPreview() { return readPreview(); },
    reportTruncated: true,
    warnings: "w".repeat(100000),
  };
  vi.mocked(request).mockResolvedValueOnce(largeAudit);
  render(<EngramFullAuditPanel projectId="one" />);
  fireEvent.click(screen.getByRole("button", { name: "Run Full Audit" }));
  await screen.findByText(/Audit healthy/);
  expect(readPreview).toHaveBeenCalledTimes(1);
  expect(screen.queryByLabelText("Audit report preview")).toBeNull();
  expect(screen.getByLabelText("Audit warnings").textContent?.length).toBeLessThan(4200);
  expect(screen.getByLabelText("Audit warnings")).toHaveTextContent("[truncated]");
  const details = screen.getByText("Audit report").closest("details")!;
  details.open = true;
  fireEvent(details, new Event("toggle"));
  expect(screen.getByText(/not the complete JSON receipt/)).toBeInTheDocument();
  const preview = screen.getByLabelText("Audit report preview").textContent!;
  expect(preview.length).toBeLessThan(16 * 1024 + 20);
  expect(preview).toBe(`${"x".repeat(16 * 1024 - 1)}\n[truncated]`);

  vi.useFakeTimers();
  try {
    let reject!: (error: Error) => void;
    vi.mocked(request).mockImplementationOnce(() => new Promise((_, fail) => { reject = fail; }));
    fireEvent.click(screen.getByRole("button", { name: "Run Full Audit" }));
    act(() => { vi.advanceTimersByTime(5000); });
    expect(screen.getByRole("status")).toHaveTextContent("5s elapsed");
    expect(readPreview).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText("Audit report preview")).toHaveTextContent("[truncated]");
    await act(async () => reject(new Error("failure-" + "z".repeat(100000))));
    expect(screen.getByRole("alert").textContent?.length).toBeLessThan(4200);
    expect(screen.getByRole("alert")).toHaveTextContent("[truncated]");
    expect(readPreview).toHaveBeenCalledTimes(1);
  } finally {
    vi.useRealTimers();
  }
});

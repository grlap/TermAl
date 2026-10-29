// Settings presentation and action boundaries for the public recovery projection.
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { EngramControlRecoverySummary } from "./api";
import { EngramControlRecoveryContext, EngramControlRecoveryPanel } from "./engram-control-recovery";
import { EngramProjectSettingsDialog } from "./EngramProjectSettingsDialog";
import { EngramPreferencesPanel } from "./preferences/engram-preferences-panel";
import type { Project, Session } from "./types";

vi.mock("./acceptance-settings-api", () => ({
  getAcceptancePolicy: vi.fn().mockResolvedValue({ available: false, readerKey: "test", error: "Unavailable" }),
  saveEvaluatorDefaults: vi.fn(), changeAcceptancePolicy: vi.fn(),
}));

const project: Project = { id: "repo", name: "Repository", rootPath: "C:/repo", engramDeclared: false };
const debt: EngramControlRecoverySummary = {
  projectId: project.id, sessionId: "original-session", operation: "turn_checkpoint",
  code: "existing_checkpoint_unconfirmed", excluded: true, settlementConfirmed: false,
  nextActions: ["inspect_session", "stop_session_if_running", "resume_local_accounting"],
};
const hostSettings = { developerName: "dev", binaryPath: "engram", home: "", bootRecoveryBudgetMs: 5000 };
afterEach(cleanup);

it("renders removed-session debt in both settings surfaces without sends or dead actions", () => {
  const onInspect = vi.fn(), onStop = vi.fn();
  const value = { entries: [debt], projects: [project], sessions: [], onInspect, onStop, stopping: {} };
  const view = render(<EngramControlRecoveryContext.Provider value={value}>
    <EngramPreferencesPanel hostSettings={hostSettings} projects={[project]} onStateUpdated={vi.fn()} />
  </EngramControlRecoveryContext.Provider>);
  const recovery = screen.getByRole("region", { name: "Engram control recovery" });
  expect(recovery).toHaveTextContent("original-session");
  expect(recovery).toHaveTextContent("Turn checkpoint");
  expect(recovery).toHaveTextContent("The original turn checkpoint has not been confirmed");
  expect(recovery).toHaveTextContent("No session action can settle this debt");
  expect(within(recovery).queryByRole("button")).not.toBeInTheDocument();
  view.rerender(<EngramControlRecoveryContext.Provider value={value}>
    <EngramProjectSettingsDialog project={project} onClose={vi.fn()} onSaved={vi.fn()} onVerified={vi.fn()} />
  </EngramControlRecoveryContext.Provider>);
  expect(screen.getByRole("dialog")).toHaveTextContent("original-session");
  expect(onInspect).not.toHaveBeenCalled();
  expect(onStop).not.toHaveBeenCalled();
});

it("requires an advertised action and matching usable target; never offers Resume or clear-debt", () => {
  const session: Session = { id: debt.sessionId, projectId: project.id, status: "active",
    name: "Original", emoji: "", agent: "Codex", workdir: "C:/repo", model: "default", preview: "", messages: [] };
  const onInspect = vi.fn(), onStop = vi.fn();
  const value = { entries: [debt], projects: [project], sessions: [session], onInspect, onStop, stopping: {} };
  const view = render(<EngramControlRecoveryContext.Provider value={value}><EngramControlRecoveryPanel /></EngramControlRecoveryContext.Provider>);
  fireEvent.click(screen.getByRole("button", { name: "Inspect session original-session" }));
  fireEvent.click(screen.getByRole("button", { name: "Stop running session original-session" }));
  expect(onInspect).toHaveBeenCalledWith(session.id);
  expect(onStop).toHaveBeenCalledWith(session.id);
  expect(screen.queryByRole("button", { name: /resume|clear/i })).not.toBeInTheDocument();
  for (const next of [
    { ...value, entries: [{ ...debt, nextActions: [] }] },
    { ...value, sessions: [{ ...session, projectId: "another" }] },
    { ...value, sessions: [] },
  ]) {
    view.rerender(<EngramControlRecoveryContext.Provider value={next}><EngramControlRecoveryPanel /></EngramControlRecoveryContext.Provider>);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByText("original-session")).toBeInTheDocument();
  }
  view.rerender(<EngramControlRecoveryContext.Provider value={{ ...value, sessions: [{ ...session, status: "idle" }] }}><EngramControlRecoveryPanel /></EngramControlRecoveryContext.Provider>);
  expect(screen.queryByRole("button", { name: "Stop running session original-session" })).not.toBeInTheDocument();
});

it.each(["__proto__", "constructor", "toString"])("renders reserved unknown reason %s safely", code => {
  render(<EngramControlRecoveryContext.Provider value={{ entries: [{ ...debt, code }], projects: [project],
    sessions: [], onInspect: vi.fn(), onStop: vi.fn(), stopping: {} }}><EngramControlRecoveryPanel /></EngramControlRecoveryContext.Provider>);
  expect(screen.getByText("The original operation's outcome remains unconfirmed.")).toBeVisible();
});

it("closes the project dialog through its existing Inspect callback", () => {
  const onClose = vi.fn(), onInspect = vi.fn();
  const session: Session = { id: debt.sessionId, projectId: project.id, status: "idle",
    name: "Original", emoji: "", agent: "Codex", workdir: "C:/repo", model: "default", preview: "", messages: [] };
  render(<EngramControlRecoveryContext.Provider value={{ entries: [debt], projects: [project],
    sessions: [session], onInspect, onStop: vi.fn(), stopping: {} }}>
    <EngramProjectSettingsDialog project={project} onClose={onClose} onSaved={vi.fn()} onVerified={vi.fn()} />
  </EngramControlRecoveryContext.Provider>);
  fireEvent.click(screen.getByRole("button", { name: "Inspect session original-session" }));
  expect(onClose).toHaveBeenCalledOnce();
  expect(onInspect).toHaveBeenCalledWith(session.id);
});

it("filters projects and confirmed settlement without exposing unknown payload fields", () => {
  const privateEntry = { ...debt, code: "C:/private/store/token", operation: "secret-request",
    routingToken: "private-token", frozenRequest: "private-payload" };
  const value = { entries: [privateEntry, { ...debt, projectId: "other" }], projects: [project], sessions: [],
    onInspect: vi.fn(), onStop: vi.fn(), stopping: {} };
  const view = render(<EngramControlRecoveryContext.Provider value={value}><EngramControlRecoveryPanel projectId={project.id} /></EngramControlRecoveryContext.Provider>);
  expect(screen.getByText("Control operation")).toBeInTheDocument();
  expect(document.body.textContent).not.toMatch(/private|secret-request/);
  expect(screen.getAllByText("original-session")).toHaveLength(1);
  view.rerender(<EngramControlRecoveryContext.Provider value={{ ...value, entries: [{ ...debt, settlementConfirmed: true }] }}><EngramControlRecoveryPanel /></EngramControlRecoveryContext.Provider>);
  expect(screen.queryByRole("region", { name: "Engram control recovery" })).not.toBeInTheDocument();
});

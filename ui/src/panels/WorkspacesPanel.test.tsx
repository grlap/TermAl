import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  StrictMode,
  Suspense,
  useLayoutEffect,
  useInsertionEffect,
  useRef,
  type ComponentProps,
  type ReactNode,
} from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WorkspaceLayoutSummary } from "../api";
import { SettingsDialogShell } from "../preferences/SettingsDialogShell";
import {
  visibleWorkspaceSummaries,
  workspaceDescriptionDomId,
  WorkspacesPanel,
  WorkspacesPanelHeaderActions,
  matchingRetainedWorkspaceSummary,
  publishLiveWorkspaceSummary,
  workspacesRefreshBusy,
  workspaceDisplayName,
  workspaceOverflowTriggerLabel,
} from "./WorkspacesPanel";
import type { WorkspaceDeleteRequest } from "../workspace-delete-request";
import { useCommittedRef } from "./use-committed-ref";
import { WorkspaceRowOverflowMenu } from "./WorkspaceRowOverflowMenu";
import {
  availableWorkspaceOverflowMenuHeight,
  isMenuItemDisabled,
  measureWorkspaceOverflowMenuPlacement,
  resolveWorkspaceOverflowMenuPlacement,
  resolveWorkspaceOverflowScroller,
  workspaceOverflowMenuMinHeight,
  workspaceOverflowMenuNeedsReveal,
} from "./workspace-overflow-menu-geometry";

function props(): ComponentProps<typeof WorkspacesPanel> {
  return {
    currentWorkspaceId: "workspace-zulu",
    summaries: [
      { id: "workspace-alpha", label: "Alpha", revision: 1, updatedAt: "yesterday", controlPanelSide: "left" },
      { id: "workspace-zulu", label: "Zulu", revision: 1, updatedAt: "today", controlPanelSide: "left" },
    ],
    deletingWorkspaceIds: [],
    error: null,
    isLoading: false,
    onDeleteWorkspace: vi.fn(() => ({ started: false, completed: Promise.resolve() })),
    onRenameWorkspace: vi.fn().mockResolvedValue(undefined),
    onRefresh: vi.fn(),
    onOpenWorkspace: vi.fn(),
  };
}

function overflowMenuProps(
  overrides: Partial<ComponentProps<typeof WorkspaceRowOverflowMenu>> = {},
): ComponentProps<typeof WorkspaceRowOverflowMenu> {
  return {
    workspaceId: "workspace-alpha",
    displayName: "Alpha",
    descriptionId: "workspace-alpha-description",
    isCurrent: false,
    isPersisted: true,
    isDeleting: false,
    isSavingLabel: false,
    open: true,
    layoutSignature: "alpha",
    onToggle: vi.fn(),
    onClose: vi.fn(),
    onRename: vi.fn(),
    onRequestDelete: vi.fn(),
    onTriggerRef: vi.fn(),
    onEscapeToTrigger: vi.fn(),
    ...overrides,
  };
}

function installQueuedAnimationFrames() {
  const queued = new Map<number, FrameRequestCallback>();
  let nextId = 1;
  const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    const id = nextId;
    nextId += 1;
    queued.set(id, callback);
    return id;
  });
  const caf = vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
    queued.delete(Number(id));
  });
  return {
    queued,
    flush() {
      const callbacks = [...queued.values()];
      queued.clear();
      for (const callback of callbacks) {
        callback(0);
      }
    },
    restore() {
      raf.mockRestore();
      caf.mockRestore();
    },
  };
}

function hangingDeleteRequest(): WorkspaceDeleteRequest {
  return { started: true, completed: new Promise(() => {}) };
}

function createDeferredDeleteRequest() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const completed = new Promise<void>((resolvePromise, rejectPromise) => {
    resolve = () => resolvePromise();
    reject = rejectPromise;
  });
  return {
    request: { started: true as const, completed },
    resolve,
    reject,
  };
}

const initialUrl = window.location.href;
afterEach(() => window.history.replaceState(null, "", initialUrl));

async function readStylesheetText(relativeFromTest: string) {
  const nodeFsModule = "node:fs";
  const nodeUrlModule = "node:url";
  const { readFileSync } = await import(nodeFsModule) as {
    readFileSync: (path: string, encoding: "utf8") => string;
  };
  const { fileURLToPath } = await import(nodeUrlModule) as {
    fileURLToPath: (url: string) => string;
  };
  const moduleUrl = import.meta.url;
  return readFileSync(fileURLToPath(new URL(relativeFromTest, moduleUrl).href), "utf8");
}

function installStylesheet(owner: string, css: string) {
  const style = document.createElement("style");
  style.dataset.testOwner = owner;
  style.textContent = css;
  document.head.append(style);
  return style;
}

function extractStandaloneCssRule(css: string, selector: string) {
  const match = new RegExp(
    `^${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} \\{`,
    "m",
  ).exec(css);
  expect(match, `test must find ${selector} in the production stylesheet`).not.toBeNull();
  const start = match!.index;
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let index = open; index < css.length; index += 1) {
    const char = css[index];
    if (char === "{") {
      depth += 1;
    } else if (char === "}") {
      depth -= 1;
      if (depth === 0) {
        return css.slice(start, index + 1);
      }
    }
  }
  throw new Error(`unclosed ${selector}`);
}

async function installProductionPanelCascade(panelCss: string) {
  const themesCss = await readStylesheetText("../themes/index.css");
  const stylesCss = await readStylesheetText("../styles.css");
  const panelRule = extractStandaloneCssRule(stylesCss, ".panel");
  const riseIn = extractStandaloneCssRule(stylesCss, "@keyframes rise-in");
  expect(panelRule, "styles.css .panel must own the rise-in animation").toMatch(/animation:\s*rise-in\b/);
  // JSDOM drops computed animation-name when the full 12k-line styles.css
  // is injected. Production order is still component CSS, then themes,
  // then the actual competing .panel / rise-in rules from styles.css.
  return [
    installStylesheet("workspaces-panel", panelCss),
    installStylesheet("themes-index", themesCss),
    installStylesheet("styles-panel-rise-in", `${panelRule}\n${riseIn}`),
  ];
}

const neverSettles = new Promise<never>(() => {});

function retainedSummaryFixture(id: string, label: string): WorkspaceLayoutSummary {
  return {
    id,
    label,
    revision: 1,
    updatedAt: "now",
    controlPanelSide: "left",
  };
}

function RetainedSummaryProbe({
  allowFallback,
  live,
  onCommit,
  ownerId,
  saving,
  suspend,
}: {
  allowFallback: boolean;
  live: WorkspaceLayoutSummary | null;
  onCommit: (read: () => {
    retained: WorkspaceLayoutSummary | null;
    shown: WorkspaceLayoutSummary | null;
    saving: boolean;
  }) => void;
  ownerId: string | null;
  saving: boolean;
  suspend: boolean;
}) {
  const retainedRef = useRef<WorkspaceLayoutSummary | null>(null);
  const savingRef = useCommittedRef(saving);
  useInsertionEffect(() => {
    publishLiveWorkspaceSummary(retainedRef, live);
  }, [live]);
  const shown = matchingRetainedWorkspaceSummary(
    live,
    retainedRef.current,
    ownerId,
    allowFallback,
  );
  useLayoutEffect(() => {
    onCommit(() => ({
      retained: retainedRef.current,
      shown,
      saving: savingRef.current,
    }));
  });
  if (suspend) {
    throw neverSettles;
  }
  return null;
}

function strictWrapper({ children }: { children: ReactNode }) {
  return <StrictMode>{children}</StrictMode>;
}

describe("WorkspacesPanel", () => {
  it("does not fall back to a retained summary for a different owner", () => {
    const retained = retainedSummaryFixture("workspace-alpha", "Alpha");
    const live = retainedSummaryFixture("workspace-beta", "Beta");
    expect(matchingRetainedWorkspaceSummary(live, retained, "workspace-beta", true)).toBe(live);
    expect(matchingRetainedWorkspaceSummary(null, retained, "workspace-alpha", true)).toBe(retained);
    expect(matchingRetainedWorkspaceSummary(null, retained, "workspace-beta", true)).toBeNull();
    expect(matchingRetainedWorkspaceSummary(null, retained, "workspace-alpha", false)).toBeNull();
  });

  it("keeps committed retained summaries and mirrors after an abandoned render", () => {
    const committed = retainedSummaryFixture("workspace-alpha", "Alpha");
    const discarded = retainedSummaryFixture("workspace-discarded", "Discarded");
    let readCommitted = () => ({
      retained: null as WorkspaceLayoutSummary | null,
      shown: null as WorkspaceLayoutSummary | null,
      saving: false,
    });
    const onCommit = (
      read: () => {
        retained: WorkspaceLayoutSummary | null;
        shown: WorkspaceLayoutSummary | null;
        saving: boolean;
      },
    ) => {
      readCommitted = read;
    };
    const view = render(
      <Suspense fallback={<span>loading</span>}>
        <RetainedSummaryProbe
          allowFallback
          live={committed}
          onCommit={onCommit}
          ownerId="workspace-alpha"
          saving={false}
          suspend={false}
        />
      </Suspense>,
      { wrapper: strictWrapper },
    );
    expect(readCommitted()).toEqual({ retained: committed, shown: committed, saving: false });

    view.rerender(
      <Suspense fallback={<span>loading</span>}>
        <RetainedSummaryProbe
          allowFallback
          live={discarded}
          onCommit={onCommit}
          ownerId="workspace-alpha"
          saving
          suspend
        />
      </Suspense>,
    );
    expect(readCommitted()).toEqual({ retained: committed, shown: committed, saving: false });

    view.rerender(
      <Suspense fallback={<span>loading</span>}>
        <RetainedSummaryProbe
          allowFallback
          live={null}
          onCommit={onCommit}
          ownerId="workspace-alpha"
          saving
          suspend={false}
        />
      </Suspense>,
    );
    expect(readCommitted()).toEqual({ retained: committed, shown: committed, saving: true });
  });

  it("pins the current workspace first even when its label sorts last", () => {
    const input = props();
    render(<WorkspacesPanel {...input} />);
    const titles = screen.getAllByRole("listitem").map((row) =>
      row.querySelector(".workspaces-panel-item-title")?.textContent,
    );
    expect(titles).toEqual(["Zulu", "Alpha"]);
    expect(visibleWorkspaceSummaries(input.summaries, "workspace-zulu").map((row) => row.id))
      .toEqual(["workspace-zulu", "workspace-alpha"]);
  });

  it("adds a synthetic current row and keeps stable id keys when the summary is missing", () => {
    const input = props();
    input.currentWorkspaceId = "workspace-missing";
    input.summaries = [
      { id: "workspace-alpha", label: "Alpha", revision: 1, updatedAt: "yesterday", controlPanelSide: "left" },
    ];
    const view = render(<WorkspacesPanel {...input} />);
    const current = screen.getByRole("button", { name: "workspace-missing (current)" });
    expect(current).toHaveAttribute("title", "workspace-missing");
    expect(within(current.closest("[role='listitem']")!).queryByRole("button", { name: /Actions for/ })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: /Open in new tab/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /Open in new tab/ })).not.toBeInTheDocument();

    view.rerender(<WorkspacesPanel {...input} summaries={[...input.summaries].reverse()} />);
    expect(screen.getByRole("button", { name: "workspace-missing (current)" })).toBeInTheDocument();
  });

  it("filters by label or id and reports a no-match state", () => {
    render(<WorkspacesPanel {...props()} />);
    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "alpha" },
    });
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Alpha" })).toBeInTheDocument();

    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "workspace-zulu" },
    });
    expect(screen.getByRole("button", { name: "Zulu (current)" })).toBeInTheDocument();

    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "no-such-workspace" },
    });
    expect(screen.queryByRole("listitem")).not.toBeInTheDocument();
    expect(screen.getByText("No workspaces match that search.")).toBeInTheDocument();
  });

  it("preserves an existing list error on mount refresh and Reloads via onRefresh", () => {
    const input = props();
    input.isLoading = true;
    input.error = "Backend needs restart";
    render(<WorkspacesPanel {...input} />);
    expect(screen.getByText("Loading saved workspaces…")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(input.onRefresh).toHaveBeenCalledTimes(1);
    expect(input.onRefresh).toHaveBeenLastCalledWith({ preserveError: true });
    fireEvent.click(screen.getByRole("button", { name: "Reload list" }));
    expect(input.onRefresh).toHaveBeenCalledTimes(2);
    expect(input.onRefresh).toHaveBeenLastCalledWith();
  });

  it("does not refetch when onRefresh identity changes, but remount and buttons use the latest callback", () => {
    const first = vi.fn();
    const second = vi.fn();
    const input = props();
    const view = render(<WorkspacesPanel {...input} onRefresh={first} />);
    expect(first).toHaveBeenCalledTimes(1);
    expect(first).toHaveBeenLastCalledWith({ preserveError: true });

    view.rerender(<WorkspacesPanel {...input} onRefresh={second} />);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).not.toHaveBeenCalled();

    view.rerender(<WorkspacesPanel {...input} onRefresh={second} error="Backend needs restart" />);
    fireEvent.click(screen.getByRole("button", { name: "Reload list" }));
    expect(second).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenLastCalledWith();

    view.unmount();
    render(<WorkspacesPanel {...input} onRefresh={second} />);
    expect(second).toHaveBeenCalledTimes(2);
    expect(second).toHaveBeenLastCalledWith({ preserveError: true });
  });

  it("keeps a rejected rename draft and restores focus on cancel", async () => {
    const input = props();
    input.onRenameWorkspace = vi.fn().mockRejectedValue(new Error("Backend needs restart"));
    render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    fireEvent.change(field, { target: { value: "Planning" } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(field).toHaveValue("Planning");
    fireEvent.keyDown(field, { key: "Escape" });
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Actions for workspace Zulu" })).toHaveFocus();
  });

  it("disables overflow actions while a delete is pending", () => {
    const input = props();
    input.deletingWorkspaceIds = ["workspace-alpha"];
    render(<WorkspacesPanel {...input} />);
    const overflow = screen.getByRole("button", {
      name: workspaceOverflowTriggerLabel("Alpha", true),
    });
    expect(overflow).toBeDisabled();
    expect(overflow).toHaveTextContent("Deleting");
    expect(overflow).toHaveAccessibleName("Deleting. Actions for workspace Alpha");
  });

  it("unlocks confirm, cancel, and escape when delete does not start", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledTimes(3);
  });

  it("unlocks confirm, cancel, and escape after explicit early delete completion", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => ({ started: true, completed: Promise.resolve() }));
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    await act(async () => {});
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    await act(async () => {});
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    await act(async () => {});
    expect(input.onDeleteWorkspace).toHaveBeenCalledTimes(3);
  });

  it("does not stick the delete UI after a synchronous callback failure", async () => {
    const input = props();
    let shouldThrow = true;
    input.onDeleteWorkspace = vi.fn(() => {
      if (shouldThrow) {
        throw new Error("sync delete failed");
      }
      return { started: false, completed: Promise.resolve() };
    });
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();

    shouldThrow = false;
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledTimes(2);
  });

  it("handles a rejected delete completion without leaving the UI stuck", async () => {
    const input = props();
    const deferred = createDeferredDeleteRequest();
    input.onDeleteWorkspace = vi.fn(() => deferred.request);
    const rejected: unknown[] = [];
    const onUnhandled = (event: PromiseRejectionEvent) => {
      rejected.push(event.reason);
      event.preventDefault();
    };
    window.addEventListener("unhandledrejection", onUnhandled);
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    try {
      await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
      await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
      await user.click(screen.getByRole("button", { name: "Confirm delete" }));
      await user.click(screen.getByRole("button", { name: "Cancel" }));
      expect(screen.getByRole("group", { name: "Delete workspace workspace-alpha" })).toBeInTheDocument();
      await act(async () => {
        deferred.reject(new Error("async delete failed"));
      });
      expect(rejected).toEqual([]);
      await user.click(screen.getByRole("button", { name: "Cancel" }));
      expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
    } finally {
      window.removeEventListener("unhandledrejection", onUnhandled);
    }
  });

  it("keeps a deferred started delete guarded until completion", async () => {
    const input = props();
    const deferred = createDeferredDeleteRequest();
    input.onDeleteWorkspace = vi.fn(() => deferred.request);
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
    expect(screen.getByRole("group", { name: "Delete workspace workspace-alpha" })).toBeInTheDocument();
    await act(async () => {
      deferred.resolve();
    });
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
  });

  it("does not let a late first-delete completion unlock a newer same-id pending delete", async () => {
    const input = props();
    const first = createDeferredDeleteRequest();
    const second = createDeferredDeleteRequest();
    let started = 0;
    input.onDeleteWorkspace = vi.fn(() => {
      started += 1;
      return started === 1 ? first.request : second.request;
    });
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha"]} />);
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={[]} />);
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledTimes(2);
    await act(async () => {
      first.resolve();
    });
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("group", { name: "Delete workspace workspace-alpha" })).toBeInTheDocument();
    await act(async () => {
      second.resolve();
    });
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
  });

  it("requires delete confirmation before calling the handler", () => {
    const input = props();
    render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    expect(screen.getByRole("group", { name: "Delete workspace workspace-alpha" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
  });

  it("describes the selected inline delete confirmation with instance-safe references", async () => {
    const input = props();
    input.summaries = [
      ...input.summaries,
      { id: "workspace-beta", label: "Beta", revision: 1, updatedAt: "earlier", controlPanelSide: "left" },
    ];
    const user = userEvent.setup();
    render(
      <>
        <WorkspacesPanel {...input} />
        <WorkspacesPanel {...input} />
      </>,
    );
    const alphaOverflows = screen.getAllByRole("button", { name: "Actions for workspace Alpha" });
    await user.click(alphaOverflows[0]);
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    const alphaConfirm = screen.getByRole("group", { name: "Delete workspace workspace-alpha" });
    expect(alphaConfirm).toHaveAccessibleDescription("Delete Alpha? This removes the saved layout.");
    expect(screen.getAllByRole("button", { name: "Beta" })).toHaveLength(2);

    const betaOverflows = screen.getAllByRole("button", { name: "Actions for workspace Beta" });
    await user.click(betaOverflows[1]);
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Beta" }));
    const betaConfirm = screen.getByRole("group", { name: "Delete workspace workspace-beta" });
    expect(screen.getByRole("group", { name: "Delete workspace workspace-alpha" })).toHaveAccessibleDescription(
      "Delete Alpha? This removes the saved layout.",
    );
    expect(betaConfirm).toHaveAccessibleDescription("Delete Beta? This removes the saved layout.");
    const describedBy = [
      screen.getByRole("group", { name: "Delete workspace workspace-alpha" }).getAttribute("aria-describedby"),
      betaConfirm.getAttribute("aria-describedby"),
    ];
    expect(describedBy[0]).toBeTruthy();
    expect(describedBy[1]).toBeTruthy();
    expect(describedBy[0]).not.toBe(describedBy[1]);
    expect(document.getElementById(describedBy[0]!)).toHaveTextContent(
      "Delete Alpha? This removes the saved layout.",
    );
    expect(document.getElementById(describedBy[1]!)).toHaveTextContent(
      "Delete Beta? This removes the saved layout.",
    );
  });

  it("keeps row and confirmation description ids distinct when a workspace id starts with confirm-", () => {
    const input = props();
    input.summaries = [
      { id: "review", label: "Review", revision: 1, updatedAt: "yesterday", controlPanelSide: "left" },
      { id: "confirm-review", label: "Confirm review", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: "workspace-zulu", label: "Zulu", revision: 1, updatedAt: "today", controlPanelSide: "left" },
    ];
    render(
      <>
        <WorkspacesPanel {...input} />
        <WorkspacesPanel {...input} />
      </>,
    );
    const reviewOverflows = screen.getAllByRole("button", { name: "Actions for workspace Review" });
    fireEvent.click(reviewOverflows[0]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete workspace Review" }));
    const confirm = screen.getByRole("group", { name: "Delete workspace review" });
    const confirmId = confirm.getAttribute("aria-describedby");
    const rowIds = screen.getAllByRole("button", { name: "Confirm review" }).map((button) =>
      button.getAttribute("aria-describedby"),
    );
    expect(confirmId).toBeTruthy();
    expect(rowIds[0]).toBeTruthy();
    expect(confirmId).not.toBe(rowIds[0]);
    expect(confirmId).not.toBe(rowIds[1]);
    expect(rowIds[0]).not.toBe(rowIds[1]);
    expect(document.getElementById(confirmId!)).toHaveTextContent(
      "Delete Review? This removes the saved layout.",
    );
    expect(document.getElementById(rowIds[0]!)).toHaveTextContent("confirm-review");
    expect(document.getElementById(rowIds[1]!)).toHaveTextContent("confirm-review");
    expect(document.querySelectorAll(`[id="${CSS.escape(confirmId!)}"]`)).toHaveLength(1);
    expect(document.querySelectorAll(`[id="${CSS.escape(rowIds[0]!)}"]`)).toHaveLength(1);
  });

  it("keeps duplicate display names distinguishable by unique workspace descriptions", async () => {
    const input = props();
    input.summaries = [
      { id: "workspace-alpha", label: "Twin", revision: 1, updatedAt: "yesterday", controlPanelSide: "left" },
      { id: "workspace-beta", label: "Twin", revision: 1, updatedAt: "earlier", controlPanelSide: "left" },
      { id: "workspace-zulu", label: "Zulu", revision: 1, updatedAt: "today", controlPanelSide: "left" },
    ];
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    const overflows = screen.getAllByRole("button", { name: "Actions for workspace Twin" });
    expect(overflows).toHaveLength(2);
    const describedBy = overflows.map((button) => button.getAttribute("aria-describedby"));
    expect(describedBy[0]).toBeTruthy();
    expect(describedBy[1]).toBeTruthy();
    expect(describedBy[0]).not.toBe(describedBy[1]);
    expect(document.getElementById(describedBy[0]!)).toHaveTextContent("workspace-alpha");
    expect(document.getElementById(describedBy[1]!)).toHaveTextContent("workspace-beta");
    await user.click(overflows[0]);
    const menu = screen.getByRole("menu", { name: "Workspace actions Twin" });
    expect(menu).toHaveAttribute("aria-describedby", describedBy[0]);
    expect(screen.getByRole("menuitem", { name: "Delete workspace Twin" })).toHaveAttribute(
      "aria-describedby",
      describedBy[0],
    );
    expect(screen.getByRole("menuitem", { name: "Open in new tab: workspace Twin" })).toHaveAttribute(
      "aria-describedby",
      describedBy[0],
    );
  });

  it("confirms and cancels the inline delete group from the keyboard", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    screen.getByRole("button", { name: "Confirm delete" }).focus();
    await user.keyboard("{Enter}");
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
  });

  it("hides new-tab and delete on the current row, including its synthetic fallback", () => {
    const input = props();
    const view = render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    expect(screen.queryByRole("menuitem", { name: "Open in new tab: workspace Zulu" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /Open in new tab/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /Delete/ })).not.toBeInTheDocument();

    view.rerender(<WorkspacesPanel {...input} summaries={input.summaries.slice(0, 1)} currentWorkspaceId="workspace-zulu" />);
    expect(screen.getByRole("button", { name: "workspace-zulu (current)" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Actions for workspace Zulu" })).not.toBeInTheDocument();
  });

  it("leaves new-tab activations to the browser", async () => {
    const input = props();
    render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    const link = screen.getByRole("menuitem", { name: "Open in new tab: workspace Alpha" });
    const sourceUrl = window.location.href;
    let preventedByProduct: boolean | undefined;
    document.addEventListener("click", (event) => {
      preventedByProduct = event.defaultPrevented;
      event.preventDefault();
    }, { once: true });
    fireEvent(link, createEvent("click", link, { bubbles: true, cancelable: true }, { EventType: "MouseEvent" }));
    expect(preventedByProduct).toBe(false);
    expect(window.location.href).toBe(sourceUrl);
    expect(input.onOpenWorkspace).not.toHaveBeenCalled();
    await act(async () => {
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    });
  });

  it("ellipsis-truncates an 80-character label instead of showing a wide id chip", () => {
    const label = "W".repeat(80);
    const input = props();
    input.summaries = [{
      id: "workspace-long-name-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
      label,
      revision: 1,
      updatedAt: "today",
      controlPanelSide: "left",
    }];
    input.currentWorkspaceId = input.summaries[0].id;
    render(<WorkspacesPanel {...input} />);
    const title = screen.getByText(label);
    expect(title).toHaveClass("workspaces-panel-item-title");
    expect(workspaceDisplayName(input.summaries[0])).toHaveLength(80);
    expect(screen.getByText(input.summaries[0].id)).toHaveClass("visually-hidden");
    expect(screen.getByRole("button", { name: `${label} (current)` })).toHaveAttribute("title", input.summaries[0].id);
  });

  it("saves a trimmed label and treats an empty value as clearing it", async () => {
    const input = props();
    let finishSave!: () => void;
    input.onRenameWorkspace = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => { finishSave = resolve; }));
    render(<WorkspacesPanel {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), { target: { value: "  Reviews  " } });
    fireEvent.click(screen.getByRole("button", { name: "Save label" }));
    expect(input.onRenameWorkspace).toHaveBeenCalledWith("workspace-zulu", "Reviews");
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    await act(async () => finishSave());
    await waitFor(() => expect(screen.queryByRole("textbox")).not.toBeInTheDocument());
  });

  it("saves a rename through a pointerdown-to-click on Save label", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    await user.clear(field);
    await user.type(field, "Planning");
    await user.click(field);
    expect(field).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save label" }));
    expect(input.onRenameWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-zulu", "Planning");
  });

  it("keeps a pending rename through Escape, outside pointer, and rejection", async () => {
    const input = props();
    let rejectSave!: (error: Error) => void;
    input.onRenameWorkspace = vi.fn(
      () => new Promise<void>((_, reject) => {
        rejectSave = reject;
      }),
    );
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    await user.clear(field);
    await user.type(field, "Planning");
    await user.click(screen.getByRole("button", { name: "Save label" }));
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    await user.keyboard("{Escape}");
    fireEvent.pointerDown(document.body);
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    await act(async () => rejectSave(new Error("Backend needs restart")));
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
  });

  it("confirms delete through a pointerdown-to-click and focuses Cancel first", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(input.onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith("workspace-alpha");
  });

  it("returns focus to the overflow trigger when delete confirmation is cancelled", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    const overflow = screen.getByRole("button", { name: "Actions for workspace Alpha" });
    await user.click(overflow);
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(overflow).toHaveFocus();
    expect(input.onDeleteWorkspace).not.toHaveBeenCalled();
  });

  it("moves overflow-menu focus with Arrow, Home, and End", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    const rename = screen.getByRole("menuitem", { name: "Rename" });
    const openTab = screen.getByRole("menuitem", { name: "Open in new tab: workspace Alpha" });
    const deleteItem = screen.getByRole("menuitem", { name: "Delete workspace Alpha" });
    await waitFor(() => expect(rename).toHaveFocus());
    await user.keyboard("{ArrowDown}");
    expect(openTab).toHaveFocus();
    await user.keyboard("{End}");
    expect(deleteItem).toHaveFocus();
    await user.keyboard("{Home}");
    expect(rename).toHaveFocus();
    await user.keyboard("{ArrowUp}");
    expect(deleteItem).toHaveFocus();
  });

  it("closes the overflow menu on outside pointer without restoring overflow focus", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    const overflow = screen.getByRole("button", { name: "Actions for workspace Alpha" });
    await user.click(overflow);
    await waitFor(() => expect(screen.getByRole("menuitem", { name: "Rename" })).toHaveFocus());
    await user.click(screen.getByRole("searchbox", { name: "Search workspaces" }));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(screen.getByRole("searchbox", { name: "Search workspaces" })).toHaveFocus();
    expect(overflow).not.toHaveFocus();
  });

  it("keeps document overflow listeners stable and calls the latest onClose", () => {
    const first = vi.fn();
    const second = vi.fn();
    const add = vi.spyOn(document, "addEventListener");
    const remove = vi.spyOn(document, "removeEventListener");
    let view: ReturnType<typeof render> | undefined;
    try {
      view = render(<WorkspaceRowOverflowMenu {...overflowMenuProps({ onClose: first })} />);
      const addedPointer = add.mock.calls.filter((call) => call[0] === "pointerdown").length;
      const addedFocus = add.mock.calls.filter((call) => call[0] === "focusin").length;
      const removedPointer = remove.mock.calls.filter((call) => call[0] === "pointerdown").length;
      const removedFocus = remove.mock.calls.filter((call) => call[0] === "focusin").length;
      view.rerender(<WorkspaceRowOverflowMenu {...overflowMenuProps({ onClose: second })} />);
      expect(add.mock.calls.filter((call) => call[0] === "pointerdown")).toHaveLength(addedPointer);
      expect(add.mock.calls.filter((call) => call[0] === "focusin")).toHaveLength(addedFocus);
      expect(remove.mock.calls.filter((call) => call[0] === "pointerdown")).toHaveLength(removedPointer);
      expect(remove.mock.calls.filter((call) => call[0] === "focusin")).toHaveLength(removedFocus);
      fireEvent.pointerDown(document.body);
      expect(first).not.toHaveBeenCalled();
      expect(second).toHaveBeenCalledTimes(1);
    } finally {
      add.mockRestore();
      remove.mockRestore();
      view?.unmount();
    }
  });

  it("cancels overflow menu rAFs on close so they cannot close a reopened menu", () => {
    const onClose = vi.fn();
    const frames = installQueuedAnimationFrames();
    try {
      const view = render(<WorkspaceRowOverflowMenu {...overflowMenuProps({ onClose })} />);
      const menu = screen.getByRole("menu");
      const overflowRoot = screen.getByRole("button", { name: "Actions for workspace Alpha" })
        .closest(".workspaces-panel-item-overflow");
      expect(overflowRoot).toBeTruthy();
      screen.getByRole("menuitem", { name: "Delete workspace Alpha" }).focus();
      fireEvent.keyDown(menu, { key: "Tab" });
      fireEvent.blur(overflowRoot!, { relatedTarget: null });
      expect(frames.queued.size).toBeGreaterThan(0);
      view.rerender(<WorkspaceRowOverflowMenu {...overflowMenuProps({ onClose, open: false })} />);
      expect(frames.queued.size).toBe(0);
      view.rerender(<WorkspaceRowOverflowMenu {...overflowMenuProps({ onClose, open: true })} />);
      onClose.mockClear();
      frames.flush();
      expect(onClose).not.toHaveBeenCalled();
      expect(screen.getByRole("menu")).toBeInTheDocument();
      view.unmount();
    } finally {
      frames.restore();
    }
  });

  it("focuses a surviving overflow after delete, or search when none remain", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => hangingDeleteRequest());
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(
      <WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha"]} />,
    );
    expect(within(screen.getByRole("group", { name: /Delete workspace/ })).getByRole("button", { name: "Deleting" })).toHaveFocus();
    view.rerender(
      <WorkspacesPanel
        {...input}
        deletingWorkspaceIds={[]}
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-alpha")}
      />,
    );
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Actions for workspace Zulu" })).toHaveFocus();
    });

    view.unmount();
    const fallback = props();
    fallback.onDeleteWorkspace = vi.fn(() => hangingDeleteRequest());
    fallback.currentWorkspaceId = "workspace-missing";
    fallback.summaries = [{
      id: "workspace-alpha",
      label: "Alpha",
      revision: 1,
      updatedAt: "yesterday",
      controlPanelSide: "left",
    }];
    const isolated = render(<WorkspacesPanel {...fallback} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    isolated.rerender(<WorkspacesPanel {...fallback} deletingWorkspaceIds={["workspace-alpha"]} />);
    isolated.rerender(<WorkspacesPanel {...fallback} deletingWorkspaceIds={[]} summaries={[]} />);
    await waitFor(() => {
      expect(screen.getByRole("searchbox", { name: "Search workspaces" })).toHaveFocus();
    });
  });

  it("does not steal focus after a failed delete and an unrelated row removal", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => hangingDeleteRequest());
    input.summaries = [
      ...input.summaries,
      { id: "workspace-beta", label: "Beta", revision: 1, updatedAt: "earlier", controlPanelSide: "left" },
    ];
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha"]} />);
    expect(screen.getByRole("group", { name: /Delete workspace/ })).toBeInTheDocument();
    expect(within(screen.getByRole("group", { name: /Delete workspace/ })).getByRole("button", { name: "Deleting" })).toHaveFocus();
    view.rerender(
      <WorkspacesPanel {...input} deletingWorkspaceIds={[]} error="Delete failed." />,
    );
    expect(screen.getByRole("group", { name: /Delete workspace/ })).toBeInTheDocument();
    const search = screen.getByRole("searchbox", { name: "Search workspaces" });
    await user.click(search);
    expect(search).toHaveFocus();
    view.rerender(
      <WorkspacesPanel
        {...input}
        deletingWorkspaceIds={[]}
        error="Delete failed."
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-beta")}
      />,
    );
    expect(search).toHaveFocus();
    expect(screen.getByRole("button", { name: "Alpha" })).toBeInTheDocument();
  });

  it("keeps injective description ids for colliding workspace ids and two panels", () => {
    const spacedId = "a b";
    const encodedLookalike = "a_20_b";
    expect(workspaceDescriptionDomId("panel-a", spacedId))
      .not.toEqual(workspaceDescriptionDomId("panel-a", encodedLookalike));
    expect(workspaceDescriptionDomId("panel-a", spacedId)).not.toMatch(/\s/);
    expect(workspaceDescriptionDomId("panel-a", encodedLookalike)).not.toMatch(/\s/);

    const input = props();
    input.currentWorkspaceId = spacedId;
    input.summaries = [
      { id: spacedId, label: "Spaced", revision: 1, updatedAt: "today", controlPanelSide: "left" },
      { id: encodedLookalike, label: "Lookalike", revision: 1, updatedAt: "today", controlPanelSide: "left" },
    ];
    render(
      <>
        <WorkspacesPanel {...input} />
        <WorkspacesPanel {...input} />
      </>,
    );
    const spaced = screen.getAllByRole("button", { name: "Spaced (current)" });
    const lookalike = screen.getAllByRole("button", { name: "Lookalike" });
    const ids = [...spaced, ...lookalike].map((button) => button.getAttribute("aria-describedby"));
    expect(new Set(ids).size).toBe(4);
    for (const id of ids) {
      expect(id).toBeTruthy();
      expect(id).not.toMatch(/\s/);
    }
    expect(document.getElementById(ids[0]!)).toHaveTextContent(spacedId);
    expect(document.getElementById(ids[1]!)).toHaveTextContent(spacedId);
    expect(document.getElementById(ids[2]!)).toHaveTextContent(encodedLookalike);
    expect(document.getElementById(ids[3]!)).toHaveTextContent(encodedLookalike);
  });

  it("keeps New here and New window enabled while only Refresh is loading", () => {
    render(
      <WorkspacesPanelHeaderActions
        isRefreshing={workspacesRefreshBusy(true, [])}
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "New workspace here" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "New window" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Refresh workspaces" })).toBeDisabled();
  });

  it("restores overflow focus after a deferred save, but not if the user moved away", async () => {
    const input = props();
    let finishSave!: () => void;
    input.onRenameWorkspace = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => {
      finishSave = resolve;
    }));
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    await user.click(screen.getByRole("button", { name: "Save label" }));
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    await act(async () => finishSave());
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Actions for workspace Zulu" })).toHaveFocus();
    });

    view.unmount();
    let finishSecond!: () => void;
    input.onRenameWorkspace = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => {
      finishSecond = resolve;
    }));
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    await user.click(screen.getByRole("button", { name: "Save label" }));
    const search = screen.getByRole("searchbox", { name: "Search workspaces" });
    await user.click(search);
    await act(async () => finishSecond());
    await waitFor(() => expect(screen.queryByRole("textbox")).not.toBeInTheDocument());
    expect(search).toHaveFocus();
  });

  it("lets the Settings shell consume Escape without closing a background draft", async () => {
    const input = props();
    const onCloseSettings = vi.fn();
    const user = userEvent.setup();
    function Harness({ settingsOpen }: { settingsOpen: boolean }) {
      return (
        <>
          <WorkspacesPanel {...input} />
          {settingsOpen ? (
            <SettingsDialogShell onClose={onCloseSettings}>
              <p>Appearance</p>
            </SettingsDialogShell>
          ) : null}
        </>
      );
    }
    const view = render(<Harness settingsOpen={false} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), {
      target: { value: "Planning" },
    });
    view.rerender(<Harness settingsOpen={true} />);
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onCloseSettings).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    expect(screen.getByRole("button", { name: "Actions for workspace Zulu" })).not.toHaveFocus();
  });

  it("ignores Escape from a composer", async () => {
    const input = props();
    const user = userEvent.setup();
    render(
      <>
        <WorkspacesPanel {...input} />
        <textarea aria-label="Composer" />
      </>,
    );
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    const composer = screen.getByRole("textbox", { name: "Composer" });
    await user.click(composer);
    await user.keyboard("{Escape}");
    expect(field).toHaveValue("Zulu");
    expect(composer).toHaveFocus();
  });

  it("ignores Escape after a consumed handler", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    const consume = (event: KeyboardEvent) => event.preventDefault();
    field.addEventListener("keydown", consume, true);
    try {
      fireEvent.keyDown(field, { key: "Escape" });
      expect(screen.getByRole("textbox", { name: "Workspace label" })).toBeInTheDocument();
    } finally {
      field.removeEventListener("keydown", consume, true);
    }
  });

  it("ignores composing Escape without a preventDefault fixture", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    const composing = fireEvent.keyDown(field, { key: "Escape", isComposing: true });
    expect(composing).toBe(true);
    expect(field).toHaveValue("Zulu");
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toBeInTheDocument();
  });

  it("keeps B's delete confirmation when pending delete A resolves first", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => hangingDeleteRequest());
    input.summaries = [
      ...input.summaries,
      { id: "workspace-beta", label: "Beta", revision: 1, updatedAt: "earlier", controlPanelSide: "left" },
    ];
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha"]} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Beta" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Beta" }));
    expect(screen.getByRole("group", { name: "Delete workspace workspace-beta" })).toBeInTheDocument();
    view.rerender(
      <WorkspacesPanel
        {...input}
        deletingWorkspaceIds={[]}
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-alpha")}
      />,
    );
    expect(screen.getByRole("group", { name: "Delete workspace workspace-beta" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Beta" })).toBeInTheDocument();
  });

  it("keeps B's pending delete when A settles after B was also confirmed", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => hangingDeleteRequest());
    input.summaries = [
      ...input.summaries,
      { id: "workspace-beta", label: "Beta", revision: 1, updatedAt: "earlier", controlPanelSide: "left" },
    ];
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha"]} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Beta" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Beta" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(
      <WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha", "workspace-beta"]} />,
    );
    expect(screen.getByRole("group", { name: "Delete workspace workspace-beta" })).toBeInTheDocument();
    view.rerender(
      <WorkspacesPanel
        {...input}
        deletingWorkspaceIds={["workspace-beta"]}
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-alpha")}
      />,
    );
    expect(screen.getByRole("group", { name: "Delete workspace workspace-beta" })).toBeInTheDocument();
    view.rerender(
      <WorkspacesPanel
        {...input}
        deletingWorkspaceIds={[]}
        summaries={input.summaries.filter((summary) => summary.id === "workspace-zulu")}
      />,
    );
    await waitFor(() => {
      expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
    });
  });

  it("moves rename input focus when the target workspace changes", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveFocus();
    expect(screen.getByRole("form", { name: "Label workspace workspace-zulu" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Workspace label" });
    expect(field).toHaveFocus();
    expect(screen.getByRole("form", { name: "Label workspace workspace-alpha" })).toBeInTheDocument();
    expect(field).toHaveValue("Alpha");
  });

  it("closes the overflow menu when Tab leaves the menu and trigger", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await waitFor(() => expect(screen.getByRole("menuitem", { name: "Rename" })).toHaveFocus());
    await user.tab();
    await user.tab();
    await user.tab();
    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
  });

  it("keeps pending-delete focus on a focusable confirm control, not body", async () => {
    const input = props();
    input.onDeleteWorkspace = vi.fn(() => hangingDeleteRequest());
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    await user.click(screen.getByRole("button", { name: "Confirm delete" }));
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={["workspace-alpha"]} />);
    const dialog = screen.getByRole("group", { name: "Delete workspace workspace-alpha" });
    const pending = within(dialog).getByRole("button", { name: "Deleting" });
    expect(pending).toHaveAttribute("aria-disabled", "true");
    expect(pending).not.toBeDisabled();
    expect(document.activeElement === pending || document.activeElement === dialog).toBe(true);
    expect(document.activeElement).not.toBe(document.body);
    view.rerender(<WorkspacesPanel {...input} deletingWorkspaceIds={[]} error="Delete failed." />);
    expect(dialog).toBeInTheDocument();
    expect(document.activeElement).not.toBe(document.body);
  });

  it("keeps the in-flight rename draft when Rename is used during save", async () => {
    const input = props();
    let rejectSave!: (error: Error) => void;
    input.onRenameWorkspace = vi.fn(
      () => new Promise<void>((_, reject) => {
        rejectSave = reject;
      }),
    );
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), {
      target: { value: "Planning" },
    });
    await user.click(screen.getByRole("button", { name: "Save label" }));
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    expect(screen.getByRole("menuitem", { name: "Rename" })).toBeDisabled();
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    expect(screen.getByRole("menuitem", { name: "Rename" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Delete workspace Alpha" })).toBeEnabled();
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    await act(async () => rejectSave(new Error("Backend needs restart")));
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    expect(screen.getByRole("button", { name: "Save label" })).toBeEnabled();
  });

  it("invokes Reload list without forwarding the click event", () => {
    const input = props();
    input.error = "Backend needs restart";
    render(<WorkspacesPanel {...input} />);
    expect(input.onRefresh).toHaveBeenCalledTimes(1);
    expect(input.onRefresh).toHaveBeenLastCalledWith({ preserveError: true });
    fireEvent.click(screen.getByRole("button", { name: "Reload list" }));
    expect(input.onRefresh).toHaveBeenCalledTimes(2);
    expect(input.onRefresh).toHaveBeenLastCalledWith();
  });

  it("keeps Reload list as a sibling of the message-only list error alert", () => {
    const input = props();
    input.error = "Backend needs restart";
    render(<WorkspacesPanel {...input} />);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Backend needs restart");
    expect(alert).not.toHaveTextContent("Reload list");
    expect(within(alert).queryByRole("button")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Reload list" }));
    expect(input.onRefresh).toHaveBeenLastCalledWith();
  });

  it("moves pending-save menu focus across enabled items only", async () => {
    const input = props();
    input.onRenameWorkspace = vi.fn(() => new Promise<void>(() => {}));
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), {
      target: { value: "Planning" },
    });
    await user.click(screen.getByRole("button", { name: "Save label" }));
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    const openTab = screen.getByRole("menuitem", { name: "Open in new tab: workspace Alpha" });
    const deleteItem = screen.getByRole("menuitem", { name: "Delete workspace Alpha" });
    const rename = screen.getByRole("menuitem", { name: "Rename" });
    expect(rename).toBeDisabled();
    await waitFor(() => expect(openTab).toHaveFocus());
    expect(rename).not.toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(deleteItem).toHaveFocus();
    await user.keyboard("{Home}");
    expect(openTab).toHaveFocus();
    await user.keyboard("{End}");
    expect(deleteItem).toHaveFocus();
    await user.keyboard("{ArrowUp}");
    expect(openTab).toHaveFocus();
  });

  it("restores rename input focus after a deferred rejection if it was lost", async () => {
    const input = props();
    let rejectSave!: (error: Error) => void;
    input.onRenameWorkspace = vi.fn(
      () => new Promise<void>((_, reject) => {
        rejectSave = reject;
      }),
    );
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), {
      target: { value: "Planning" },
    });
    await user.click(screen.getByRole("button", { name: "Save label" }));
    expect(document.activeElement).not.toBe(screen.getByRole("textbox", { name: "Workspace label" }));
    await act(async () => rejectSave(new Error("Backend needs restart")));
    const field = await screen.findByRole("textbox", { name: "Workspace label" });
    expect(field).toHaveValue("Planning");
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    await waitFor(() => expect(field).toHaveFocus());
    expect(screen.getByRole("button", { name: "Save label" })).toBeEnabled();
  });

  it("does not steal rename focus after rejection if the user moved away", async () => {
    const input = props();
    let rejectSave!: (error: Error) => void;
    input.onRenameWorkspace = vi.fn(
      () => new Promise<void>((_, reject) => {
        rejectSave = reject;
      }),
    );
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), {
      target: { value: "Planning" },
    });
    await user.click(screen.getByRole("button", { name: "Save label" }));
    await user.click(screen.getByRole("searchbox", { name: "Search workspaces" }));
    await act(async () => rejectSave(new Error("Backend needs restart")));
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    expect(screen.getByRole("searchbox", { name: "Search workspaces" })).toHaveFocus();
  });

  it("lets Escape reach search after a filtered-away row is actually removed", async () => {
    const input = props();
    input.onRenameWorkspace = vi.fn(() => new Promise<void>(() => {}));
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "zulu" },
    });
    expect(screen.getByRole("form", { name: "Label workspace workspace-alpha" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Alpha" })).not.toBeInTheDocument();
    view.rerender(
      <WorkspacesPanel
        {...input}
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-alpha")}
      />,
    );
    const search = screen.getByRole("searchbox", { name: "Search workspaces" });
    search.focus();
    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveFocus();
    expect(screen.queryByRole("button", { name: "Actions for workspace Alpha" })).not.toBeInTheDocument();
  });

  it("keeps a pending save after the target row disappears and still ignores Escape", async () => {
    const input = props();
    let rejectSave!: (error: Error) => void;
    input.onRenameWorkspace = vi.fn(
      () => new Promise<void>((_, reject) => {
        rejectSave = reject;
      }),
    );
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Workspace label" }), {
      target: { value: "Planning" },
    });
    await user.click(screen.getByRole("button", { name: "Save label" }));
    view.rerender(
      <WorkspacesPanel
        {...input}
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-alpha")}
      />,
    );
    expect(screen.getByRole("form", { name: "Label workspace workspace-alpha" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    fireEvent.keyDown(screen.getByRole("region", { name: "Workspaces" }), { key: "Escape" });
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    await act(async () => rejectSave(new Error("Backend needs restart")));
    expect(await screen.findByRole("alert")).toHaveTextContent("Backend needs restart");
    expect(screen.getByRole("textbox", { name: "Workspace label" })).toHaveValue("Planning");
    expect(screen.getByRole("button", { name: "Save label" })).toBeEnabled();
  });

  it("does not let a removed confirmation swallow Escape or restore a missing trigger", async () => {
    const input = props();
    const user = userEvent.setup();
    const view = render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    expect(screen.getByRole("group", { name: "Delete workspace workspace-alpha" })).toBeInTheDocument();
    view.rerender(
      <WorkspacesPanel
        {...input}
        summaries={input.summaries.filter((summary) => summary.id !== "workspace-alpha")}
      />,
    );
    expect(screen.queryByRole("group", { name: /Delete workspace/ })).not.toBeInTheDocument();
    const search = screen.getByRole("searchbox", { name: "Search workspaces" });
    search.focus();
    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveFocus();
    expect(screen.queryByRole("button", { name: "Actions for workspace Alpha" })).not.toBeInTheDocument();
  });

  it("keeps Refresh busy while a delete is outstanding even if the list GET is idle", () => {
    const input = props();
    input.isLoading = false;
    input.deletingWorkspaceIds = ["workspace-alpha"];
    render(<WorkspacesPanel {...input} />);
    expect(screen.getByRole("status")).toHaveTextContent("Deleting");
    expect(screen.queryByText("Loading saved workspaces…")).not.toBeInTheDocument();
  });

  it("prefers GET loading copy when a list fetch overlaps an outstanding delete", () => {
    const input = props();
    input.isLoading = true;
    input.deletingWorkspaceIds = ["workspace-alpha"];
    render(<WorkspacesPanel {...input} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading saved workspaces…");
    expect(screen.getByRole("status")).not.toHaveTextContent("Deleting");
  });

  it("places overflow menus by available space instead of forcing up", () => {
    const scroller = { top: 0, bottom: 200 };
    expect(resolveWorkspaceOverflowMenuPlacement(
      { top: 8, bottom: 32 },
      scroller,
      80,
    )).toBe("down");
    expect(resolveWorkspaceOverflowMenuPlacement(
      { top: 80, bottom: 104 },
      scroller,
      80,
    )).toBe("down");
    expect(resolveWorkspaceOverflowMenuPlacement(
      { top: 168, bottom: 192 },
      scroller,
      80,
    )).toBe("up");
    expect(resolveWorkspaceOverflowMenuPlacement(
      { top: 8, bottom: 32 },
      scroller,
      180,
    )).toBe("down");
    expect(resolveWorkspaceOverflowMenuPlacement(
      { top: 160, bottom: 184 },
      scroller,
      180,
    )).toBe("up");
    expect(availableWorkspaceOverflowMenuHeight(
      { top: 8, bottom: 32 },
      scroller,
      "up",
    )).toBe(0);
    expect(availableWorkspaceOverflowMenuHeight(
      { top: 8, bottom: 32 },
      scroller,
      "up",
      8,
      32,
    )).toBe(32);
    expect(resolveWorkspaceOverflowMenuPlacement(
      { top: 40, bottom: 64 },
      { top: 0, bottom: 80 },
      180,
      8,
      { canScrollUp: false, canScrollDown: true },
    )).toBe("down");
    expect(workspaceOverflowMenuNeedsReveal(
      { top: 36, bottom: 116 },
      scroller,
    )).toBe(false);
    expect(workspaceOverflowMenuNeedsReveal(
      { top: -8, bottom: 32 },
      scroller,
      { canScrollUp: false, canScrollDown: true },
    )).toBe(false);
    const outer = document.createElement("div");
    const inner = document.createElement("div");
    outer.style.overflowY = "auto";
    outer.append(inner);
    document.body.append(outer);
    try {
      expect(resolveWorkspaceOverflowScroller(inner)).toBe(outer);
    } finally {
      outer.remove();
    }
  });

  it("measures overflow placement without mutating menu class or maxHeight", () => {
    const scroller = document.createElement("div");
    scroller.style.overflow = "auto";
    const trigger = document.createElement("button");
    const menu = document.createElement("div");
    menu.className = "workspaces-panel-menu panel";
    menu.style.maxHeight = "24px";
    scroller.append(trigger, menu);
    document.body.append(scroller);
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this === scroller) {
        return { x: 0, y: 0, top: 0, left: 0, right: 320, bottom: 200, width: 320, height: 200, toJSON() {} };
      }
      if (this === trigger) {
        return { x: 280, y: 8, top: 8, left: 280, right: 312, bottom: 32, width: 32, height: 24, toJSON() {} };
      }
      if (this === menu) {
        return { x: 180, y: 40, top: 40, left: 180, right: 320, bottom: 120, width: 140, height: 80, toJSON() {} };
      }
      return originalRect.call(this);
    });
    try {
      const measured = measureWorkspaceOverflowMenuPlacement(menu, trigger);
      expect(measured.placement).toBe("down");
      expect(measured.maxHeight).toBeGreaterThan(0);
      expect(menu.className).toBe("workspaces-panel-menu panel");
      expect(menu.classList.contains("workspaces-panel-menu-up")).toBe(false);
      expect(menu.style.maxHeight).toBe("24px");
    } finally {
      rectSpy.mockRestore();
      scroller.remove();
    }
  });

  it("falls back to a usable min-height when there is no overflow ancestor", () => {
    const trigger = document.createElement("button");
    const menu = document.createElement("div");
    const item = document.createElement("button");
    item.setAttribute("role", "menuitem");
    menu.append(item);
    document.body.append(trigger, menu);
    const itemRect = vi.spyOn(item, "getBoundingClientRect").mockReturnValue({
      x: 0, y: 8, top: 8, left: 0, right: 140, bottom: 44, width: 140, height: 36, toJSON() {},
    });
    try {
      expect(resolveWorkspaceOverflowScroller(menu)).toBeNull();
      const measured = measureWorkspaceOverflowMenuPlacement(menu, trigger);
      expect(measured.placement).toBe("down");
      expect(measured.maxHeight).toBeGreaterThan(0);
      expect(measured.maxHeight).toBeGreaterThanOrEqual(workspaceOverflowMenuMinHeight(menu));
    } finally {
      itemRect.mockRestore();
      trigger.remove();
      menu.remove();
    }
  });

  it("keeps an open overflow menu usable without an overflow ancestor", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    const menu = screen.getByRole("menu");
    const maxHeight = Number.parseFloat(menu.style.maxHeight);
    expect(Number.isFinite(maxHeight)).toBe(true);
    expect(maxHeight).toBeGreaterThan(0);
    expect(screen.getByRole("menuitem", { name: "Rename" })).toHaveFocus();
    expect(screen.getByRole("menuitem", { name: "Open in new tab: workspace Alpha" })).toBeEnabled();
    expect(screen.getByRole("menuitem", { name: "Delete workspace Alpha" })).toBeEnabled();
    await act(async () => {
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    });
    expect(screen.getByRole("menu")).toBe(menu);
    expect(Number.parseFloat(screen.getByRole("menu").style.maxHeight)).toBe(maxHeight);
  });

  it("sizes overflow menu min-height from the focused item plus padding and borders", () => {
    const menu = document.createElement("div");
    menu.style.boxSizing = "border-box";
    menu.style.paddingTop = "6px";
    menu.style.paddingBottom = "6px";
    menu.style.borderTopWidth = "2px";
    menu.style.borderBottomWidth = "2px";
    menu.style.borderTopStyle = "solid";
    menu.style.borderBottomStyle = "solid";
    menu.style.fontSize = "16px";
    const first = document.createElement("button");
    first.setAttribute("role", "menuitem");
    first.disabled = true;
    const second = document.createElement("button");
    second.setAttribute("role", "menuitem");
    menu.append(first, second);
    document.body.append(menu);
    const itemRect = (top: number, height: number) => ({
      x: 0, y: top, top, left: 0, right: 140, bottom: top + height, width: 140, height, toJSON() {},
    });
    const firstRect = vi.spyOn(first, "getBoundingClientRect").mockReturnValue(itemRect(8, 36));
    const secondRect = vi.spyOn(second, "getBoundingClientRect").mockReturnValue(itemRect(44, 36));
    try {
      expect(isMenuItemDisabled(first)).toBe(true);
      expect(isMenuItemDisabled(second)).toBe(false);
      expect(workspaceOverflowMenuMinHeight(menu)).toBe(36 + 36 + 6 + 6 + 2 + 2);
      first.disabled = false;
      expect(isMenuItemDisabled(first)).toBe(false);
      expect(workspaceOverflowMenuMinHeight(menu)).toBe(36 + 6 + 6 + 2 + 2);
      expect(workspaceOverflowMenuMinHeight(menu)).toBeGreaterThan(36);
      menu.style.boxSizing = "content-box";
      expect(workspaceOverflowMenuMinHeight(menu)).toBe(36);
    } finally {
      firstRect.mockRestore();
      secondRect.mockRestore();
      menu.remove();
    }
  });

  it("keeps a persistent polite live status across idle, loading, and deleting", () => {
    const input = props();
    const view = render(<WorkspacesPanel {...input} />);
    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent("");
    expect(screen.getAllByRole("status")).toHaveLength(1);

    view.rerender(<WorkspacesPanel {...input} isLoading />);
    expect(screen.getByRole("status")).toBe(status);
    expect(status).toHaveTextContent("Loading saved workspaces…");
    expect(screen.getAllByRole("status")).toHaveLength(1);

    view.rerender(
      <WorkspacesPanel {...input} isLoading={false} deletingWorkspaceIds={["workspace-alpha"]} />,
    );
    expect(screen.getByRole("status")).toBe(status);
    expect(status).toHaveTextContent("Deleting");
    expect(screen.getAllByRole("status")).toHaveLength(1);

    view.rerender(<WorkspacesPanel {...input} isLoading={false} deletingWorkspaceIds={[]} />);
    expect(screen.getByRole("status")).toBe(status);
    expect(status).toHaveTextContent("");
  });

  it("flips a last-row overflow menu up when it would overflow the panel scroller", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.classList.contains("control-panel-body")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: 160, width: 320, height: 160, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-alpha") {
        return {
          x: 280, y: 130, top: 130, left: 280, right: 312, bottom: 154, width: 32, height: 24, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        if (this.classList.contains("workspaces-panel-menu-up")) {
          return {
            x: 180, y: 20, top: 20, left: 180, right: 320, bottom: 122, width: 140, height: 102, toJSON() {},
          };
        }
        return {
          x: 180, y: 158, top: 158, left: 180, right: 320, bottom: 318, width: 140, height: 160, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      render(
        <div className="control-panel-body" style={{ overflow: "auto" }}>
          <WorkspacesPanel {...input} />
        </div>,
      );
      await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
      await waitFor(() => {
        expect(screen.getByRole("menu")).toHaveClass("workspaces-panel-menu-up");
      });
      expect(scrollIntoView).not.toHaveBeenCalled();
    } finally {
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("opts the overflow menu out of the global panel rise-in animation", async () => {
    const panelCss = await readStylesheetText("./WorkspacesPanel.css");
    const injected = await installProductionPanelCascade(panelCss);
    const plainPanel = document.createElement("div");
    plainPanel.className = "panel";
    document.body.append(plainPanel);
    const input = props();
    const user = userEvent.setup();
    try {
      // JSDOM leaves animationName empty and only fills the animation shorthand.
      expect(getComputedStyle(plainPanel).animation, "ordinary .panel must receive rise-in from styles.css").toContain("rise-in");
      render(<WorkspacesPanel {...input} />);
      await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
      const menu = screen.getByRole("menu");
      expect(getComputedStyle(menu).animation).toBe("none");
    } finally {
      plainPanel.remove();
      for (const style of injected) {
        style.remove();
      }
    }
  });

  it("focuses the first overflow menu item with preventScroll when the menu fits below", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const focus = vi.spyOn(HTMLElement.prototype, "focus");
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.classList.contains("control-panel-body")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: 200, width: 320, height: 200, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-zulu") {
        return {
          x: 280, y: 8, top: 8, left: 280, right: 312, bottom: 32, width: 32, height: 24, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        return {
          x: 180, y: 36, top: 36, left: 180, right: 320, bottom: 116, width: 140, height: 80, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      render(
        <div className="control-panel-body" style={{ overflow: "auto" }}>
          <WorkspacesPanel {...input} />
        </div>,
      );
      await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
      const firstItem = screen.getByRole("menuitem", { name: "Rename" });
      expect(screen.getByRole("menu")).not.toHaveClass("workspaces-panel-menu-up");
      expect(scrollIntoView).not.toHaveBeenCalled();
      const preventScrollIndex = focus.mock.calls.findIndex((call) => (
        Boolean(call[0] && typeof call[0] === "object" && "preventScroll" in call[0] && call[0].preventScroll)
      ));
      expect(preventScrollIndex).toBeGreaterThan(-1);
      expect(focus.mock.instances[preventScrollIndex]).toBe(firstItem);
      expect(focus).toHaveBeenCalledWith({ preventScroll: true });
    } finally {
      scrollIntoView.mockRestore();
      focus.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("reveals a still-clipped menu after final placement", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.hasAttribute("data-overflow-boundary")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: 80, width: 320, height: 80, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-alpha") {
        return {
          x: 280, y: 28, top: 28, left: 280, right: 312, bottom: 52, width: 32, height: 24, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        const maxHeight = Number.parseFloat(this.style.maxHeight);
        const height = Number.isFinite(maxHeight) && maxHeight > 0 ? maxHeight : 80;
        const top = this.classList.contains("workspaces-panel-menu-up") ? 28 - 8 - height : 52 + 8;
        return {
          x: 180, y: top, top, left: 180, right: 320, bottom: top + height, width: 140, height, toJSON() {},
        };
      }
      if (this.getAttribute("role") === "menuitem") {
        return {
          x: 180, y: 0, top: 0, left: 180, right: 320, bottom: 40, width: 140, height: 40, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      const scroller = document.createElement("div");
      scroller.setAttribute("data-overflow-boundary", "");
      scroller.style.overflow = "auto";
      Object.defineProperty(scroller, "scrollTop", { configurable: true, value: 12 });
      Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 200 });
      Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 80 });
      const view = render(<WorkspacesPanel {...input} />, { container: document.body.appendChild(scroller) });
      await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
      await waitFor(() => {
        const menu = screen.getByRole("menu");
        expect(menu.style.maxHeight).toBe(`${workspaceOverflowMenuMinHeight(menu)}px`);
      });
      expect(screen.getByRole("menu")).not.toHaveClass("workspaces-panel-menu-up");
      expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });
      expect(
        scrollIntoView.mock.instances.some(
          (node) => node instanceof HTMLElement && node.classList.contains("workspaces-panel-menu"),
        ),
      ).toBe(true);
      view.unmount();
      scroller.remove();
    } finally {
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("places overflow menus against a scrollable ancestor that is not control-panel-body", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.hasAttribute("data-overflow-boundary")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: 200, width: 320, height: 200, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-zulu") {
        return {
          x: 280, y: 8, top: 8, left: 280, right: 312, bottom: 32, width: 32, height: 24, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        return {
          x: 180, y: 36, top: 36, left: 180, right: 320, bottom: 116, width: 140, height: 80, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      render(
        <div data-overflow-boundary="" style={{ overflow: "auto" }}>
          <WorkspacesPanel {...input} />
        </div>,
      );
      await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
      expect(screen.getByRole("menu")).not.toHaveClass("workspaces-panel-menu-up");
      expect(scrollIntoView).not.toHaveBeenCalled();
    } finally {
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("recomputes an open overflow menu when the scroller resizes without stealing focus", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const geometry = {
      scrollerBottom: 200,
      triggerTop: 8,
      triggerBottom: 32,
    };
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const focus = vi.spyOn(HTMLElement.prototype, "focus");
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.classList.contains("control-panel-body")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: geometry.scrollerBottom,
          width: 320, height: geometry.scrollerBottom, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-zulu") {
        return {
          x: 280, y: geometry.triggerTop, top: geometry.triggerTop, left: 280,
          right: 312, bottom: geometry.triggerBottom, width: 32,
          height: geometry.triggerBottom - geometry.triggerTop, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        const maxHeight = Number.parseFloat(this.style.maxHeight);
        const height = Number.isFinite(maxHeight) && maxHeight > 0 ? maxHeight : 80;
        const top = this.classList.contains("workspaces-panel-menu-up")
          ? geometry.triggerTop - 8 - height
          : geometry.triggerBottom + 8;
        return {
          x: 180, y: top, top, left: 180, right: 320, bottom: top + height, width: 140, height, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      render(
        <div className="control-panel-body" style={{ overflow: "auto" }}>
          <WorkspacesPanel {...input} />
        </div>,
      );
      await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
      const menu = screen.getByRole("menu");
      const firstItem = screen.getByRole("menuitem", { name: "Rename" });
      expect(menu).not.toHaveClass("workspaces-panel-menu-up");
      expect(firstItem).toHaveFocus();
      focus.mockClear();
      geometry.scrollerBottom = 160;
      geometry.triggerTop = 130;
      geometry.triggerBottom = 154;
      act(() => {
        window.dispatchEvent(new Event("resize"));
      });
      expect(screen.getByRole("menu")).toHaveClass("workspaces-panel-menu-up");
      expect(screen.getByRole("menu")).toBe(menu);
      expect(firstItem).toHaveFocus();
      expect(focus.mock.calls.some((call) => (
        Boolean(call[0] && typeof call[0] === "object" && "preventScroll" in call[0] && call[0].preventScroll)
      ))).toBe(false);
    } finally {
      focus.mockRestore();
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("remeasures an open overflow menu on user scroll without revealing", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const geometry = {
      scrollerBottom: 200,
      triggerTop: 8,
      triggerBottom: 32,
    };
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.classList.contains("control-panel-body")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: geometry.scrollerBottom,
          width: 320, height: geometry.scrollerBottom, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-zulu") {
        return {
          x: 280, y: geometry.triggerTop, top: geometry.triggerTop, left: 280,
          right: 312, bottom: geometry.triggerBottom, width: 32,
          height: geometry.triggerBottom - geometry.triggerTop, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        const maxHeight = Number.parseFloat(this.style.maxHeight);
        const height = Number.isFinite(maxHeight) && maxHeight > 0 ? maxHeight : 80;
        const top = this.classList.contains("workspaces-panel-menu-up")
          ? geometry.triggerTop - 8 - height
          : geometry.triggerBottom + 8;
        return {
          x: 180, y: top, top, left: 180, right: 320, bottom: top + height, width: 140, height, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      const view = render(
        <div className="control-panel-body" style={{ overflow: "auto" }}>
          <WorkspacesPanel {...input} />
        </div>,
      );
      const scroller = view.container.querySelector(".control-panel-body");
      expect(scroller).toBeInstanceOf(HTMLElement);
      Object.defineProperty(scroller, "scrollTop", { configurable: true, writable: true, value: 0 });
      Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 400 });
      Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
      await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
      const menu = screen.getByRole("menu");
      expect(menu).not.toHaveClass("workspaces-panel-menu-up");
      const fittedMaxHeight = menu.style.maxHeight;
      expect(fittedMaxHeight).not.toBe("");
      scrollIntoView.mockClear();
      geometry.scrollerBottom = 160;
      geometry.triggerTop = 130;
      geometry.triggerBottom = 154;
      (scroller as HTMLElement).scrollTop = 40;
      act(() => {
        scroller!.dispatchEvent(new Event("scroll"));
      });
      expect(screen.getByRole("menu")).toHaveClass("workspaces-panel-menu-up");
      expect(screen.getByRole("menu")).toBe(menu);
      expect(menu.style.maxHeight).not.toBe(fittedMaxHeight);
      expect(scrollIntoView).not.toHaveBeenCalled();
    } finally {
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("ignores the reveal-induced scroll until the owned reveal rAF resets", () => {
    const frames = installQueuedAnimationFrames();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const geometry = {
      scrollerBottom: 80,
      triggerTop: 28,
      triggerBottom: 52,
    };
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.hasAttribute("data-overflow-boundary")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: geometry.scrollerBottom,
          width: 320, height: geometry.scrollerBottom, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-alpha") {
        return {
          x: 280, y: geometry.triggerTop, top: geometry.triggerTop, left: 280,
          right: 312, bottom: geometry.triggerBottom, width: 32, height: 24, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        const maxHeight = Number.parseFloat(this.style.maxHeight);
        const height = Number.isFinite(maxHeight) && maxHeight > 0 ? maxHeight : 80;
        const top = this.classList.contains("workspaces-panel-menu-up") ? geometry.triggerTop - 8 - height : geometry.triggerBottom + 8;
        return {
          x: 180, y: top, top, left: 180, right: 320, bottom: top + height, width: 140, height, toJSON() {},
        };
      }
      if (this.getAttribute("role") === "menuitem") {
        return {
          x: 180, y: 0, top: 0, left: 180, right: 320, bottom: 40, width: 140, height: 40, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      const scroller = document.createElement("div");
      scroller.setAttribute("data-overflow-boundary", "");
      scroller.style.overflow = "auto";
      Object.defineProperty(scroller, "scrollTop", { configurable: true, writable: true, value: 12 });
      Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 200 });
      Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 80 });
      document.body.append(scroller);
      const view = render(
        <WorkspaceRowOverflowMenu {...overflowMenuProps()} />,
        { container: scroller },
      );
      expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });
      scrollIntoView.mockClear();
      const maxHeightAfterReveal = screen.getByRole("menu").style.maxHeight;
      act(() => {
        scroller.dispatchEvent(new Event("scroll"));
      });
      expect(scrollIntoView).not.toHaveBeenCalled();
      expect(screen.getByRole("menu").style.maxHeight).toBe(maxHeightAfterReveal);
      frames.flush();
      geometry.triggerTop = 8;
      geometry.triggerBottom = 32;
      geometry.scrollerBottom = 200;
      act(() => {
        scroller.dispatchEvent(new Event("scroll"));
      });
      expect(scrollIntoView).not.toHaveBeenCalled();
      expect(screen.getByRole("menu").style.maxHeight).not.toBe(maxHeightAfterReveal);
      view.unmount();
      scroller.remove();
    } finally {
      frames.restore();
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("recomputes an open overflow menu when the list layout signature changes", async () => {
    const input = props();
    const user = userEvent.setup();
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    const geometry = {
      scrollerBottom: 160,
      triggerTop: 130,
      triggerBottom: 154,
      unclampedHeight: 160,
    };
    const scrollIntoView = vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => {});
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function mockRect(this: HTMLElement) {
      if (this.classList.contains("control-panel-body")) {
        return {
          x: 0, y: 0, top: 0, left: 0, right: 320, bottom: geometry.scrollerBottom,
          width: 320, height: geometry.scrollerBottom, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-overflow-trigger")
        && this.getAttribute("data-workspace-overflow") === "workspace-alpha") {
        return {
          x: 280, y: geometry.triggerTop, top: geometry.triggerTop, left: 280,
          right: 312, bottom: geometry.triggerBottom, width: 32,
          height: geometry.triggerBottom - geometry.triggerTop, toJSON() {},
        };
      }
      if (this.classList.contains("workspaces-panel-menu")) {
        const height = geometry.unclampedHeight;
        const top = this.classList.contains("workspaces-panel-menu-up")
          ? geometry.triggerTop - 8 - height
          : geometry.triggerBottom + 8;
        return {
          x: 180, y: top, top, left: 180, right: 320, bottom: top + height, width: 140, height, toJSON() {},
        };
      }
      return originalRect.call(this);
    });
    try {
      const view = render(
        <div className="control-panel-body" style={{ overflow: "auto" }}>
          <WorkspacesPanel {...input} />
        </div>,
      );
      await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
      await waitFor(() => {
        expect(screen.getByRole("menu")).toHaveClass("workspaces-panel-menu-up");
      });
      geometry.triggerTop = 8;
      geometry.triggerBottom = 32;
      geometry.unclampedHeight = 80;
      view.rerender(
        <div className="control-panel-body" style={{ overflow: "auto" }}>
          <WorkspacesPanel
            {...input}
            summaries={[
              ...input.summaries,
              { id: "workspace-beta", label: "Beta", revision: 1, updatedAt: "now", controlPanelSide: "left" },
            ]}
          />
        </div>,
      );
      await waitFor(() => {
        expect(screen.getByRole("menu")).not.toHaveClass("workspaces-panel-menu-up");
      });
      expect(screen.getByRole("menuitem", { name: "Rename" })).toHaveFocus();
    } finally {
      scrollIntoView.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("restores surviving overflow after Cancel or Escape when the row is filtered out", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "zulu" },
    });
    expect(screen.queryByRole("button", { name: "Actions for workspace Alpha" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Actions for workspace Zulu" })).toHaveFocus();
    });

    await user.click(screen.getByRole("button", { name: "Actions for workspace Zulu" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "alpha" },
    });
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Workspace label" }), { key: "Escape" });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Actions for workspace Alpha" })).toHaveFocus();
    });
  });

  it("does not steal search focus when Escape cancels a filtered rename from search", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const search = screen.getByRole("searchbox", { name: "Search workspaces" });
    fireEvent.change(search, { target: { value: "zulu" } });
    search.focus();
    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveFocus();
    expect(screen.queryByRole("form", { name: "Label workspace workspace-alpha" })).not.toBeInTheDocument();
  });

  it("restores surviving overflow after cancelling a filtered delete confirmation", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    await user.click(screen.getByRole("menuitem", { name: "Delete workspace Alpha" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search workspaces" }), {
      target: { value: "zulu" },
    });
    expect(screen.queryByRole("button", { name: "Actions for workspace Alpha" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Actions for workspace Zulu" })).toHaveFocus();
    });
  });

  it("keeps a menu item activatable after a null-relatedTarget blur and an animation frame", async () => {
    const input = props();
    const user = userEvent.setup();
    render(<WorkspacesPanel {...input} />);
    await user.click(screen.getByRole("button", { name: "Actions for workspace Alpha" }));
    const rename = screen.getByRole("menuitem", { name: "Rename" });
    await waitFor(() => expect(rename).toHaveFocus());
    fireEvent.blur(rename, { relatedTarget: null });
    await act(async () => {
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    });
    expect(screen.getByRole("menu", { name: "Workspace actions Alpha" })).toBeInTheDocument();
    fireEvent.click(rename);
    expect(screen.getByRole("form", { name: "Label workspace workspace-alpha" })).toBeInTheDocument();
  });
});

describe("WorkspacesPanelHeaderActions", () => {
  it("uses the latest Refresh callback after identity change", () => {
    const first = vi.fn();
    const second = vi.fn();
    const view = render(
      <WorkspacesPanelHeaderActions
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={first}
      />,
    );
    view.rerender(
      <WorkspacesPanelHeaderActions
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={second}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Refresh workspaces" }));
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledWith();
  });

  it("invokes Refresh without forwarding the click event", () => {
    const onRefresh = vi.fn();
    render(
      <WorkspacesPanelHeaderActions
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={onRefresh}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Refresh workspaces" }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
    expect(onRefresh).toHaveBeenCalledWith();
  });

  it("disables only Refresh for GET or outstanding deletes and stays idle otherwise", () => {
    const view = render(
      <WorkspacesPanelHeaderActions
        isRefreshing={workspacesRefreshBusy(true, [])}
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "Refresh workspaces" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "New workspace here" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "New window" })).toBeEnabled();

    view.rerender(
      <WorkspacesPanelHeaderActions
        isRefreshing={workspacesRefreshBusy(false, ["workspace-alpha"])}
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "Refresh workspaces" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "New workspace here" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "New window" })).toBeEnabled();

    view.rerender(
      <WorkspacesPanelHeaderActions
        isRefreshing={workspacesRefreshBusy(false, [])}
        onOpenNewWorkspaceHere={vi.fn()}
        onOpenNewWorkspaceWindow={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "Refresh workspaces" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "New workspace here" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "New window" })).toBeEnabled();
  });
});

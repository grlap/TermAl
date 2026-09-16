// Memory browsing through real Work UI with fixture responses, no live stores.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { readProjectWork, readWorkMemories, type WorkMemoryResponse, type WorkMemorySource } from "../work-visualizer-api";
import { WorkMemories } from "./WorkMemories";
import { WorkPanel } from "./WorkPanel";
import { ApiRequestError } from "../api-request";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkMemories: vi.fn() }));
function page(source: WorkMemorySource, key = "guide"): WorkMemoryResponse {
  return { source, state: "ready", message: "Ready", readerId: source === "engram" ? "reader" : null, items: [{ key, summary: "Short summary", body: null, revision: source === "engram" ? 2 : null, rememberedAt: null, actor: null }], nextAfter: null, omitted: 0, exhausted: true, observedAt: "now" };
}
const region = (source: string) => screen.getByRole("region", { name: `${source} memories` });
describe("Work memories", () => {
  beforeEach(() => {
    vi.resetAllMocks(); localStorage.clear();
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source) => page(source));
  });

  it("only reads memories when their separate Work view is opened", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [], page: null, beads: null, readerId: null, observedAt: "now" });
    render(<WorkPanel projects={[{ id: "one", name: "One" }] as Project[]} focusedProjectId="one" />);
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(1));
    expect(readWorkMemories).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Memories" }));
    await waitFor(() => expect(readWorkMemories).toHaveBeenCalledTimes(2));
    expect(await within(region("engram")).findByRole("button", { name: "guide" })).toBeInTheDocument();
    expect(within(region("beads")).getByRole("button", { name: "guide" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Label (source filter)" })).not.toBeInTheDocument();
  });

  it("reads full text on demand, renders it inertly and restores focus on close", async () => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query) => {
      const result = page(source);
      if (query.key) result.items[0]!.body = "<script>deleteEverything()</script>\nDo not execute this memory";
      return result;
    });
    render(<WorkMemories projectId="one" />);
    const button = await within(region("engram")).findByRole("button", { name: "guide" });
    fireEvent.click(button);
    const details = within(region("engram")).getByRole("region", { name: "guide full memory" });
    expect(button).toHaveAttribute("aria-expanded", "true");
    expect(button.closest("li")).toContainElement(details);
    expect(await within(details).findByText(/<script>deleteEverything/)).toBeInTheDocument();
    expect(document.querySelector("script")).toBeNull();
    expect(readWorkMemories).toHaveBeenLastCalledWith("one", "engram", { key: "guide", readerId: "reader" }, expect.any(AbortSignal));
    fireEvent.keyDown(details, { key: "Escape" });
    expect(within(region("engram")).queryByRole("region", { name: "guide full memory" })).not.toBeInTheDocument();
    expect(button).toHaveAttribute("aria-expanded", "false");
    expect(button).toHaveFocus();
  });

  it("keeps the other source visible on failure and searches both only on submit", async () => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source) => {
      if (source === "engram") throw new Error("Engram unavailable");
      return page(source);
    });
    render(<WorkMemories projectId="one" />);
    await screen.findByText(/Engram unavailable/);
    expect(within(region("beads")).getByRole("button", { name: "guide" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "Search memories" }), { target: { value: "store safety" } });
    expect(readWorkMemories).toHaveBeenCalledTimes(2);
    fireEvent.click(screen.getByRole("button", { name: "Search memories" }));
    await waitFor(() => expect(readWorkMemories).toHaveBeenCalledTimes(4));
    expect(readWorkMemories).toHaveBeenCalledWith("one", "beads", { search: "store safety" }, expect.any(AbortSignal));
  });

  it("pages Engram and refuses a mismatched reader without discarding Beads", async () => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query) => {
      const result = page(source, query.after ? "later" : "guide");
      if (source === "engram") {
        result.nextAfter = query.after ? null : "guide";
        result.exhausted = !!query.after;
        if (query.after) result.readerId = "changed";
      }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    await screen.findByText(/Memory listing changed/);
    expect(within(region("engram")).queryByRole("button", { name: "guide" })).not.toBeInTheDocument();
    expect(within(region("beads")).getByRole("button", { name: "guide" })).toBeInTheDocument();
  });

  it("appends a valid Engram continuation without rereading Beads", async () => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query) => {
      const result = page(source, query.after ? "later" : "guide");
      if (source === "engram" && !query.after) { result.nextAfter = "guide"; result.exhausted = false; }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    expect(await within(region("engram")).findByRole("button", { name: "later" })).toBeInTheDocument();
    expect(within(region("engram")).getByRole("button", { name: "guide" })).toBeInTheDocument();
    expect(within(region("engram")).getByText(/2 loaded/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more engram memories" })).not.toBeInTheDocument();
    expect(vi.mocked(readWorkMemories).mock.calls.filter(call => call[1] === "beads")).toHaveLength(1);
  });

  it("renders pages progressively, requests one page at a time, and loads through the final page", async () => {
    let finish!: (value: WorkMemoryResponse) => void;
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query) => {
      if (source === "beads") return page(source);
      if (query.after === "guide") return new Promise(resolve => { finish = resolve; });
      const result = page(source, query.after ? "last" : "guide");
      if (!query.after) { result.nextAfter = "guide"; result.exhausted = false; }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    expect(await within(region("engram")).findByRole("button", { name: "guide" })).toBeInTheDocument();
    expect(within(region("engram")).getByText(/1 loaded · Loading more/)).toBeInTheDocument();
    expect(readWorkMemories).toHaveBeenCalledTimes(3);
    const second = page("engram", "middle"); second.nextAfter = "middle"; second.exhausted = false;
    await act(async () => finish(second));
    await within(region("engram")).findByRole("button", { name: "last" });
    expect(within(region("engram")).getByText(/3 loaded · All returned memories loaded/)).toBeInTheDocument();
    expect(readWorkMemories).toHaveBeenCalledTimes(4);
  });

  it.each(["cursor", "duplicate", "empty"])("stops an invalid %s continuation without looping", async kind => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query) => {
      const result = page(source, query.after ? "later" : "guide");
      if (source === "engram") {
        result.nextAfter = "guide"; result.exhausted = false;
        if (query.after) {
          if (kind === "duplicate") { result.items = page(source).items; result.nextAfter = null; result.exhausted = true; }
          if (kind === "empty") { result.items = []; result.nextAfter = "new-cursor"; }
        }
      }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    await screen.findByText(/Memory listing changed/);
    expect(within(region("engram")).queryByRole("button", { name: "guide" })).not.toBeInTheDocument();
    expect(readWorkMemories).toHaveBeenCalledTimes(3);
  });

  it.each([429, 502, 409])("stops on HTTP %s and only discards stale-reader data", async status => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query) => {
      if (query.after) throw new ApiRequestError("request-failed", "Read failed", { status });
      const result = page(source);
      if (source === "engram") { result.nextAfter = "guide"; result.exhausted = false; }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    await screen.findByText(/Loading stopped: Read failed/);
    expect(!!within(region("engram")).queryByRole("button", { name: "guide" })).toBe(status !== 409);
    expect(readWorkMemories).toHaveBeenCalledTimes(3);
    expect(within(region("beads")).getByRole("button", { name: "guide" })).toBeInTheDocument();
  });

  it("aborts a pending continuation on leaving without requesting another page", async () => {
    let finish!: (value: WorkMemoryResponse) => void;
    let continuationSignal: AbortSignal | undefined;
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query, signal) => {
      if (query.after) { continuationSignal = signal; return new Promise(resolve => { finish = resolve; }); }
      const result = page(source);
      if (source === "engram") { result.nextAfter = "guide"; result.exhausted = false; }
      return result;
    });
    const { unmount } = render(<WorkMemories projectId="one" />);
    await waitFor(() => expect(continuationSignal).toBeDefined());
    unmount();
    expect(continuationSignal?.aborted).toBe(true);
    const late = page("engram", "later"); late.nextAfter = "later"; late.exhausted = false;
    await act(async () => finish(late));
    expect(readWorkMemories).toHaveBeenCalledTimes(3);
  });

  it("expands rows independently, cancels collapsed bodies, and displays honest date metadata", async () => {
    let bodySignal: AbortSignal | undefined;
    let finish!: (value: WorkMemoryResponse) => void;
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source, query, signal) => {
      const result = page(source);
      if (query.key) { bodySignal = signal; return new Promise(resolve => { finish = resolve; }); }
      if (source === "engram") {
        result.items[0]!.rememberedAt = "2026-09-16T10:14:33Z";
        result.items.push(...page(source, "other").items);
      }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    const guide = await within(region("engram")).findByRole("button", { name: "guide" });
    expect(within(region("engram")).getByText(/Revision date:/)).toBeInTheDocument();
    expect(region("engram").querySelector("time[datetime='2026-09-16T10:14:33Z']")).not.toBeNull();
    expect(within(region("beads")).getByText("Beads does not provide memory dates.")).toBeInTheDocument();
    guide.focus(); fireEvent.click(guide);
    expect(guide).toHaveFocus();
    const firstSignal = bodySignal;
    const finishFirst = finish;
    const other = within(region("engram")).getByRole("button", { name: "other" });
    fireEvent.click(other);
    expect(guide).toHaveAttribute("aria-expanded", "true");
    expect(other).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(guide);
    expect(firstSignal?.aborted).toBe(true);
    expect(bodySignal?.aborted).toBe(false);
    const late = page("engram"); late.items[0]!.body = "Stale collapsed body";
    await act(async () => finishFirst(late));
    expect(screen.queryByText("Stale collapsed body")).not.toBeInTheDocument();
    expect(guide).toHaveAttribute("aria-expanded", "false");
    expect(other).toHaveAttribute("aria-expanded", "true");
  });

  it("aborts old project reads and fences late responses", async () => {
    let finish!: (value: WorkMemoryResponse) => void;
    let oldSignal: AbortSignal | undefined;
    vi.mocked(readWorkMemories).mockImplementation(async (project, source, _query, signal) => {
      if (project === "old" && source === "engram") { oldSignal = signal; return new Promise(resolve => { finish = resolve; }); }
      return page(source, project);
    });
    const { rerender } = render(<WorkMemories projectId="old" />);
    rerender(<WorkMemories projectId="new" />);
    expect(oldSignal?.aborted).toBe(true);
    await act(async () => finish(page("engram", "stale")));
    expect(await within(region("engram")).findByRole("button", { name: "new" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "stale" })).not.toBeInTheDocument();
  });

  it("shows empty versus unavailable distinctly and discloses omitted matches", async () => {
    vi.mocked(readWorkMemories).mockImplementation(async (_project, source) => {
      const result = page(source); result.items = [];
      if (source === "beads") { result.state = "unavailable"; result.message = "No .beads directory"; }
      else { result.omitted = 9; result.exhausted = false; }
      return result;
    });
    render(<WorkMemories projectId="one" />);
    expect(await screen.findByText("No matching project memories in engram.")).toBeInTheDocument();
    expect(screen.getByText(/9 more matches omitted/)).toBeInTheDocument();
    expect(screen.getByText(/unavailable: No .beads/)).toBeInTheDocument();
    expect(screen.queryByText("No matching project memories in beads.")).not.toBeInTheDocument();
  });
});

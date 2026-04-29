import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  RenderedDiffView,
  composeRenderedDiffMarkdown,
  composeRenderedDiffRegionMarkdown,
} from "./rendered-diff-view";
import type { SourceRenderableRegion } from "../source-renderers";

vi.mock("../message-cards", () => ({
  // The real `MarkdownContent` does mermaid / KaTeX / link plumbing
  // that's outside this navigation test's contract. Mock it to a
  // simple `<pre>` so each region's body is a stable target the
  // assertions can find.
  MarkdownContent: ({ markdown }: { markdown: string }) => (
    <pre data-testid="rendered-diff-markdown">{markdown}</pre>
  ),
}));

function makeRegion(overrides: Partial<SourceRenderableRegion> = {}): SourceRenderableRegion {
  return {
    id: "region-1",
    renderer: "markdown",
    sourceStartLine: 1,
    sourceEndLine: 5,
    sourceText: "body",
    displayText: "body",
    editable: false,
    ...overrides,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("RenderedDiffView", () => {
  it("renders each region in its own section with a stable change-region index attribute", () => {
    const regions: SourceRenderableRegion[] = [
      makeRegion({
        id: "region-mermaid",
        renderer: "mermaid",
        sourceStartLine: 5,
        sourceEndLine: 9,
        displayText: "flowchart TD\n  A --> B",
      }),
      makeRegion({
        id: "region-md",
        renderer: "markdown",
        sourceStartLine: 12,
        sourceEndLine: 14,
        displayText: "## Heading\n\nBody text.",
      }),
    ];

    render(
      <RenderedDiffView
        appearance="dark"
        documentPath="/repo/file.ts"
        isCompleteDocument
        regions={regions}
        workspaceRoot="/repo"
      />,
    );

    const sections = document.querySelectorAll<HTMLElement>(
      "[data-rendered-diff-region-index]",
    );
    expect(sections).toHaveLength(2);
    expect(sections[0]?.getAttribute("data-rendered-diff-region-index")).toBe("0");
    expect(sections[1]?.getAttribute("data-rendered-diff-region-index")).toBe("1");
    // Each region's `Lines N–M` header is rendered in JSX (not in the
    // markdown body) so it can carry the data-attribute and serve as
    // a scroll target.
    expect(sections[0]?.textContent).toContain("Lines 5–9");
    expect(sections[1]?.textContent).toContain("Lines 12–14");
  });

  it("starts at Region 1 of N and wraps prev/next at the boundaries", async () => {
    const regions = [
      makeRegion({ id: "r-1", sourceStartLine: 1, sourceEndLine: 3 }),
      makeRegion({ id: "r-2", sourceStartLine: 7, sourceEndLine: 9 }),
      makeRegion({ id: "r-3", sourceStartLine: 12, sourceEndLine: 18 }),
    ];

    render(
      <RenderedDiffView
        appearance="dark"
        documentPath={null}
        isCompleteDocument
        regions={regions}
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByText("Region 1 of 3")).toBeInTheDocument();

    // Stub scrollIntoView so the test can record nav intent without a
    // real layout. Apply to every region so wrap-around still works.
    const scrollIntoViewMock = vi.fn();
    document
      .querySelectorAll<HTMLElement>("[data-rendered-diff-region-index]")
      .forEach((section) => {
        section.scrollIntoView = scrollIntoViewMock;
      });

    const nextButton = screen.getByRole("button", { name: "Next region" });
    const prevButton = screen.getByRole("button", { name: "Previous region" });

    // 1 → 2
    await act(async () => {
      nextButton.click();
    });
    expect(screen.getByText("Region 2 of 3")).toBeInTheDocument();
    expect(scrollIntoViewMock).toHaveBeenLastCalledWith({ block: "center" });

    // 2 → 3
    await act(async () => {
      nextButton.click();
    });
    expect(screen.getByText("Region 3 of 3")).toBeInTheDocument();

    // 3 → wraps to 1
    await act(async () => {
      nextButton.click();
    });
    expect(screen.getByText("Region 1 of 3")).toBeInTheDocument();

    // 1 → wraps to 3
    await act(async () => {
      prevButton.click();
    });
    expect(screen.getByText("Region 3 of 3")).toBeInTheDocument();

    // Each navigation that crossed an index boundary scrolled — the
    // initial mount intentionally does NOT scroll so the parent's
    // restored scroll position is preserved.
    expect(scrollIntoViewMock).toHaveBeenCalledTimes(4);
  });

  it("reports `No rendered regions` and disables nav buttons when the region set is empty", () => {
    render(
      <RenderedDiffView
        appearance="dark"
        documentPath={null}
        isCompleteDocument
        regions={[]}
        workspaceRoot="/repo"
      />,
    );

    expect(screen.getByText("No rendered regions")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Previous region" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Next region" })).toBeDisabled();
  });

  it("shows the Patch-only disclaimer when isCompleteDocument is false", () => {
    render(
      <RenderedDiffView
        appearance="dark"
        documentPath={null}
        isCompleteDocument={false}
        regions={[makeRegion({ id: "r-1" })]}
        workspaceRoot="/repo"
      />,
    );
    expect(screen.getByText(/Patch-only rendering/)).toBeInTheDocument();
    expect(screen.getByText("Patch preview")).toBeInTheDocument();
  });
});

describe("composeRenderedDiffRegionMarkdown", () => {
  it("wraps a mermaid region in a fenced ```mermaid block", () => {
    const region = makeRegion({
      renderer: "mermaid",
      displayText: "flowchart TD\n  A --> B\n",
    });
    expect(composeRenderedDiffRegionMarkdown(region)).toBe(
      "```mermaid\nflowchart TD\n  A --> B\n```",
    );
  });

  it("wraps a math region in $$ delimiters", () => {
    const region = makeRegion({
      renderer: "math",
      displayText: "x^2 + y^2 = z^2\n",
    });
    expect(composeRenderedDiffRegionMarkdown(region)).toBe(
      "$$\nx^2 + y^2 = z^2\n$$",
    );
  });

  it("returns the raw display text for a markdown region", () => {
    const region = makeRegion({
      renderer: "markdown",
      displayText: "## Heading\n\nBody.",
    });
    expect(composeRenderedDiffRegionMarkdown(region)).toBe(
      "## Heading\n\nBody.",
    );
  });
});

describe("composeRenderedDiffMarkdown (legacy whole-document assembler)", () => {
  it("emits the `**Lines N–M**` header before each region's body", () => {
    const result = composeRenderedDiffMarkdown([
      makeRegion({
        renderer: "mermaid",
        sourceStartLine: 5,
        sourceEndLine: 9,
        displayText: "flowchart TD",
      }),
      makeRegion({
        id: "region-2",
        renderer: "markdown",
        sourceStartLine: 12,
        sourceEndLine: 14,
        displayText: "## Heading",
      }),
    ]);
    expect(result).toContain("**Lines 5–9**");
    expect(result).toContain("**Lines 12–14**");
    expect(result).toContain("```mermaid");
    expect(result).toContain("## Heading");
  });

  it("returns an empty string for an empty region list", () => {
    expect(composeRenderedDiffMarkdown([])).toBe("");
  });
});

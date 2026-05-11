import { render, screen } from "@testing-library/react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { RenderedDiffView } from "./rendered-diff-view";
import type { SourceRenderableRegion } from "../source-renderers";

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: vi.fn((id: string) =>
      Promise.resolve({
        diagramType: "flowchart",
        svg: `<svg data-testid="mermaid-svg" id="${id}"><text>diagram</text></svg>`,
      }),
    ),
  },
}));

const mermaidRenderMock = vi.mocked(mermaid.render);

function makeRegion(
  overrides: Partial<SourceRenderableRegion> = {},
): SourceRenderableRegion {
  return {
    id: "region-1",
    renderer: "mermaid",
    sourceStartLine: 1,
    sourceEndLine: 3,
    sourceText: "body",
    displayText: "body",
    editable: false,
    ...overrides,
  };
}

beforeEach(() => {
  mermaidRenderMock.mockClear();
});

describe("RenderedDiffView render budgets", () => {
  it("applies Mermaid document caps across split rendered regions", () => {
    const regions = Array.from({ length: 21 }, (_value, index) =>
      makeRegion({
        id: `mermaid-${index}`,
        renderer: "mermaid",
        sourceStartLine: index * 4 + 1,
        sourceEndLine: index * 4 + 3,
        displayText: `flowchart TD\n  A${index} --> B${index}`,
      }),
    );

    render(
      <RenderedDiffView
        appearance="dark"
        documentPath="/repo/docs.rs"
        isCompleteDocument
        regions={regions}
        workspaceRoot="/repo"
      />,
    );

    expect(
      screen.getAllByText(
        "Mermaid render skipped: document has 21 diagrams; the render budget is 20.",
      ),
    ).toHaveLength(21);
    expect(mermaidRenderMock).not.toHaveBeenCalled();
  });

  it("applies math document caps across split rendered regions", () => {
    const regions = Array.from({ length: 101 }, (_value, index) =>
      makeRegion({
        id: `math-${index}`,
        renderer: "math",
        sourceStartLine: index + 1,
        sourceEndLine: index + 1,
        displayText: `x_{${index}} + y_{${index}}`,
      }),
    );

    const { container } = render(
      <RenderedDiffView
        appearance="dark"
        documentPath="/repo/docs.rs"
        isCompleteDocument
        regions={regions}
        workspaceRoot="/repo"
      />,
    );

    const renderedMath = container.querySelectorAll(".math.math-display");
    expect(renderedMath).toHaveLength(101);
    for (const element of renderedMath) {
      expect(element.classList.contains("math-render-skipped")).toBe(true);
      expect(element.getAttribute("data-markdown-serialization")).toBe("skip");
      expect(element.getAttribute("title")).toMatch(/Math render skipped/i);
    }
  });
});

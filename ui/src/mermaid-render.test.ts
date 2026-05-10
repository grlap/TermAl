import { afterEach, describe, expect, it, vi } from "vitest";

import {
  getMermaidDiagramFrameStyle,
  isMermaidErrorVisualizationSvg,
  renderTermalMermaidDiagram,
  type MermaidModule,
} from "./mermaid-render";

const MERMAID_ERROR_SVG =
  '<svg aria-roledescription="error"><g class="error-icon"></g><text>Syntax error in text</text></svg>';

describe("mermaid-render", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("detects Mermaid error visualizations without matching valid diagram text", () => {
    expect(
      isMermaidErrorVisualizationSvg(
        '<svg aria-roledescription="error"><text>boom</text></svg>',
      ),
    ).toBe(true);
    expect(
      isMermaidErrorVisualizationSvg(
        '<svg><g class="error-icon"></g><text>Syntax error in text</text></svg>',
      ),
    ).toBe(true);
    expect(
      isMermaidErrorVisualizationSvg(
        '<svg><text>Syntax error in text is a label</text></svg>',
      ),
    ).toBe(false);
    expect(isMermaidErrorVisualizationSvg('<svg><g class="error-icon"></g></svg>')).toBe(
      false,
    );
  });

  it("keeps Mermaid frame scrollbar slack out of fit-to-frame mode", () => {
    const svg = '<svg viewBox="0 0 300 80"><text>ok</text></svg>';

    expect(getMermaidDiagramFrameStyle(svg)).toMatchObject({
      aspectRatio: "302 / 104",
      height: "auto",
      maxWidth: "100%",
      width: "302px",
    });
    expect(getMermaidDiagramFrameStyle(svg, { fitToFrame: true })).toMatchObject({
      aspectRatio: "302 / 80",
      height: "auto",
      maxWidth: "100%",
      width: "302px",
    });
  });

  it("cleans Mermaid temporary DOM nodes when an error SVG is rethrown", async () => {
    const diagramId = "termal-mermaid-test";
    const render = vi.fn(async (id: string) => {
      const wrapper = document.createElement("div");
      wrapper.id = `d${id}`;
      const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      svg.id = id;
      wrapper.appendChild(svg);
      document.body.appendChild(wrapper);
      return {
        diagramType: "flowchart",
        svg: MERMAID_ERROR_SVG,
      };
    });
    const mermaid = {
      initialize: vi.fn(),
      render,
    } as unknown as MermaidModule;

    await expect(
      renderTermalMermaidDiagram(mermaid, diagramId, "flowchart TD\nA-->B", "light"),
    ).rejects.toThrow("Mermaid syntax error");

    expect(document.getElementById(`d${diagramId}`)).toBeNull();
    expect(document.getElementById(diagramId)).toBeNull();
    expect(render).toHaveBeenCalledWith(diagramId, "flowchart TD\nA-->B");
  });
});

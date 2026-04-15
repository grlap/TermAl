import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import mermaid from "mermaid";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { MarkdownContent, areMarkdownLineMarkersEqual } from "./message-cards";

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: vi.fn(async (id: string) => ({
      diagramType: "flowchart",
      svg: `<svg data-testid="mermaid-svg" id="${id}"><text>diagram</text></svg>`,
    })),
  },
}));

const mermaidInitializeMock = vi.mocked(mermaid.initialize);
const mermaidRenderMock = vi.mocked(mermaid.render);

beforeEach(() => {
  mermaidInitializeMock.mockClear();
  mermaidRenderMock.mockClear();
  mermaidRenderMock.mockResolvedValue({
    diagramType: "flowchart",
    svg: '<svg data-testid="mermaid-svg"><text>diagram</text></svg>',
  });
});

describe("MarkdownContent inline file links", () => {
  it("renders inline code file references as clickable links when the source callback exists", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay clickable."
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const link = screen.getByRole("link", { name: "experience.tex.#L63" });
    expect(link).toHaveClass("inline-code-link");
    expect(link).toHaveAttribute("draggable", "false");

    fireEvent.click(link);

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("renders inline code file references as plain code without the source callback", () => {
    render(
      <MarkdownContent
        markdown="Text like `experience.tex.#L63` should stay plain code."
        workspaceRoot="/repo"
      />,
    );

    expect(
      screen.queryByRole("link", { name: "experience.tex.#L63" }),
    ).toBeNull();
    expect(
      screen.getByText("experience.tex.#L63", { selector: "code" }).closest("a"),
    ).toBeNull();
  });

  it("preserves inline code link DOM nodes when the source callback identity changes", () => {
    const firstOnOpenSourceLink = vi.fn();
    const secondOnOpenSourceLink = vi.fn();
    const markdown = "Text like `experience.tex.#L63` should stay clickable.";
    const { rerender } = render(
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={firstOnOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const firstLink = screen.getByRole("link", { name: "experience.tex.#L63" });
    const firstCode = firstLink.querySelector("code");

    rerender(
      <MarkdownContent
        markdown={markdown}
        onOpenSourceLink={secondOnOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    const secondLink = screen.getByRole("link", { name: "experience.tex.#L63" });
    expect(secondLink).toBe(firstLink);
    expect(secondLink.querySelector("code")).toBe(firstCode);

    fireEvent.click(secondLink);

    expect(firstOnOpenSourceLink).not.toHaveBeenCalled();
    expect(secondOnOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("renders Markdown images as non-draggable", () => {
    render(<MarkdownContent markdown="![Diagram](./diagram.png)" workspaceRoot="/repo" />);

    expect(screen.getByRole("img", { name: "Diagram" })).toHaveAttribute("draggable", "false");
  });
});

describe("MarkdownContent Mermaid diagrams", () => {
  it("renders Mermaid fenced blocks as diagrams", async () => {
    render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    expect(await screen.findByTestId("mermaid-svg")).toBeInTheDocument();
    expect(mermaidInitializeMock).toHaveBeenNthCalledWith(1, {
      darkMode: true,
      flowchart: {
        defaultRenderer: "dagre-wrapper",
        diagramPadding: 2,
        nodeSpacing: 24,
        padding: 4,
        rankSpacing: 30,
        useMaxWidth: false,
        wrappingWidth: 140,
      },
      htmlLabels: true,
      securityLevel: "strict",
      startOnLoad: false,
      theme: "dark",
      themeCSS: expect.stringContaining("border-radius: 24px"),
    });
    expect(mermaidInitializeMock).toHaveBeenNthCalledWith(2, {
      flowchart: {
        defaultRenderer: "dagre-wrapper",
        diagramPadding: 2,
        nodeSpacing: 24,
        padding: 4,
        rankSpacing: 30,
        useMaxWidth: false,
        wrappingWidth: 140,
      },
      htmlLabels: true,
      securityLevel: "strict",
      startOnLoad: false,
      themeCSS: expect.stringContaining("border-radius: 24px"),
    });
    expect(mermaidRenderMock).toHaveBeenCalledWith(
      expect.stringMatching(/^termal-mermaid-\d+$/),
      "flowchart TD\n  A --> B",
    );
  });

  it("uses light Mermaid colors when Markdown appears in a light editor", async () => {
    render(
      <MarkdownContent
        appearance="light"
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
      />,
    );

    expect(await screen.findByTestId("mermaid-svg")).toBeInTheDocument();
    expect(mermaidInitializeMock).toHaveBeenNthCalledWith(1, expect.objectContaining({
      darkMode: false,
      theme: "default",
    }));
  });

  it("renders Mermaid source without rewriting labels", async () => {
    render(
      <MarkdownContent
        markdown={[
          "```mermaid",
          "flowchart TD",
          "  Start --> Detect{Contains Mermaid fence?}",
          "```",
        ].join("\n")}
      />,
    );

    expect(await screen.findByTestId("mermaid-svg")).toBeInTheDocument();
    expect(mermaidRenderMock).toHaveBeenCalledWith(
      expect.stringMatching(/^termal-mermaid-\d+$/),
      "flowchart TD\n  Start --> Detect{Contains Mermaid fence?}",
    );
  });

  it("keeps Mermaid fenced blocks as source when diagram rendering is disabled", () => {
    const { container } = render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
        renderMermaidDiagrams={false}
      />,
    );

    expect(container.querySelector(".mermaid-diagram-block")).toBeNull();
    expect(container.querySelector("code.language-mermaid")?.textContent).toBe(
      "flowchart TD\n  A --> B",
    );
    expect(mermaidRenderMock).not.toHaveBeenCalled();
  });

  it("falls back to the Mermaid source when rendering fails", async () => {
    mermaidRenderMock.mockRejectedValueOnce(new Error("syntax error"));

    const { container } = render(
      <MarkdownContent
        markdown={["```mermaid", "flowchart TD", "  A -->", "```"].join("\n")}
      />,
    );

    expect(await screen.findByText("Mermaid render failed: syntax error")).toBeInTheDocument();
    expect(container.querySelector("code.language-mermaid")?.textContent).toBe("flowchart TD\n  A -->");
  });
});

describe("MarkdownContent document links", () => {
  it("resolves document-relative Markdown links with anchors and Windows roots", () => {
    const onOpenSourceLink = vi.fn();

    render(
      <MarkdownContent
        documentPath="C:/repo/docs/features/intro.md"
        markdown={[
          "[Sibling](./guide.md#L10)",
          "[Parent](../README.md#L12C3)",
          "[Absolute](C:/repo/docs/api.md#L7)",
        ].join("\n\n")}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="C:/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "Sibling" }));
    fireEvent.click(screen.getByRole("link", { name: "Parent" }));
    fireEvent.click(screen.getByRole("link", { name: "Absolute" }));

    expect(onOpenSourceLink).toHaveBeenNthCalledWith(1, {
      path: "C:\\repo\\docs\\features\\guide.md",
      line: 10,
      openInNewTab: false,
    });
    expect(onOpenSourceLink).toHaveBeenNthCalledWith(2, {
      path: "C:\\repo\\docs\\README.md",
      line: 12,
      column: 3,
      openInNewTab: false,
    });
    expect(onOpenSourceLink).toHaveBeenNthCalledWith(3, {
      path: "C:/repo/docs/api.md",
      line: 7,
      openInNewTab: false,
    });
  });

  it("does not create nested anchors for inline code inside Markdown links", () => {
    const { container } = render(
      <MarkdownContent
        markdown={"[prefix `lib/models/foo.rs` suffix](https://example.com)"}
        onOpenSourceLink={vi.fn()}
        workspaceRoot="/repo"
      />,
    );

    expect(container.querySelectorAll("a a")).toHaveLength(0);
    expect(screen.getByText("lib/models/foo.rs", { selector: "code" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /prefix lib\/models\/foo\.rs suffix/ })).toHaveAttribute(
      "href",
      "https://example.com",
    );
  });
});

describe("MarkdownContent line numbers", () => {
  it("renders source line numbers only when requested", async () => {
    const markdown = ["# Title", "", "Body text.", "", "- First item"].join("\n");

    const { container, rerender } = render(
      <MarkdownContent markdown={markdown} startLineNumber={40} />,
    );

    expect(container.querySelector("[data-markdown-line-start]")).toBeNull();
    expect(container.querySelector(".markdown-line-gutter")).toBeNull();
    expect(container.querySelector(".markdown-copy-shell-with-line-numbers")).toBeNull();

    rerender(
      <MarkdownContent markdown={markdown} showLineNumbers startLineNumber={40} />,
    );

    await waitFor(() => {
      expect(container.querySelector('[data-markdown-line-start="40"]')).not.toBeNull();
      expect(container.querySelector('[data-markdown-line-start="42"]')).not.toBeNull();
      expect(container.querySelector('[data-markdown-line-start="44"]')).not.toBeNull();
      expect(
        container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='40']"),
      ).toHaveTextContent("40");
    });
    expect(container.querySelector(".markdown-copy")).not.toHaveTextContent("40");
    expect(container.querySelector("ul[data-markdown-line-start]")).toBeNull();
    expect(container.querySelector('li[data-markdown-line-start="44"]')).not.toBeNull();
  });

  it("labels multi-line fenced code blocks with the rendered source range", async () => {
    const { container } = render(
      <MarkdownContent
        markdown={["Intro", "", "```ts", "const a = 1;", "const b = 2;", "```"].join("\n")}
        showLineNumbers
        startLineNumber={20}
      />,
    );

    await waitFor(() => {
      const codeBlock = container.querySelector("pre[data-markdown-line-start='22']");
      expect(codeBlock).not.toBeNull();
      expect(codeBlock).toHaveAttribute("data-markdown-line-range", "22-25");
    });
  });

  it("does not emit duplicate line markers for nested block elements", async () => {
    const { container } = render(
      <MarkdownContent markdown={"- Item\n  > Nested quote\n"} showLineNumbers />,
    );

    await waitFor(() => {
      const markers = container.querySelectorAll("[data-markdown-line-start]");
      expect(markers).toHaveLength(1);
      expect(markers[0]).toHaveAttribute("data-markdown-line-start", "1");
    });
  });

  it("keeps the line-number observer stable across search-only rerenders", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    let observeCount = 0;
    let disconnectCount = 0;
    class ResizeObserverMock {
      observe() {
        observeCount += 1;
      }
      disconnect() {
        disconnectCount += 1;
      }
    }
    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;

    try {
      const markdown = ["# Title", "", "Body text."].join("\n");
      const { container, rerender } = render(
        <MarkdownContent markdown={markdown} showLineNumbers searchQuery="" />,
      );

      await waitFor(() => {
        expect(container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='1']")).not.toBeNull();
      });
      expect(observeCount).toBe(1);
      expect(disconnectCount).toBe(0);

      rerender(
        <MarkdownContent markdown={markdown} showLineNumbers searchQuery="Body" />,
      );

      expect(observeCount).toBe(1);
      expect(disconnectCount).toBe(0);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
    }
  });

  it("updates gutter positions from ResizeObserver geometry changes", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    let resizeCallback: ResizeObserverCallback | null = null;
    let bodyTop = 130;

    class ResizeObserverMock {
      constructor(callback: ResizeObserverCallback) {
        resizeCallback = callback;
      }
      observe() {}
      disconnect() {}
    }

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const top = element.classList.contains("markdown-copy")
        ? 100
        : element.dataset.markdownLineStart === "3"
          ? bodyTop
          : 100;
      return {
        bottom: top + 20,
        height: 20,
        left: 0,
        right: 200,
        top,
        width: 200,
        x: 0,
        y: top,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const { container } = render(
        <MarkdownContent markdown={"# Title\n\nBody text."} showLineNumbers />,
      );

      await waitFor(() => {
        expect(container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='3']")).toHaveStyle({
          top: "40px",
        });
      });

      bodyTop = 154;
      await act(async () => {
        resizeCallback?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
      });

      await waitFor(() => {
        expect(container.querySelector(".markdown-line-gutter [data-markdown-gutter-line='3']")).toHaveStyle({
          top: "64px",
        });
      });
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("compares line marker arrays by line, range, and top", () => {
    const marker = { line: 4, range: "4-6", top: 20 };

    expect(areMarkdownLineMarkersEqual([marker], [{ line: 4, range: "4-6", top: 20 }])).toBe(true);
    expect(areMarkdownLineMarkersEqual([marker], [{ line: 5, range: "4-6", top: 20 }])).toBe(false);
    expect(areMarkdownLineMarkersEqual([marker], [{ line: 4, range: "4", top: 20 }])).toBe(false);
    expect(areMarkdownLineMarkersEqual([marker], [{ line: 4, range: "4-6", top: 21 }])).toBe(false);
  });
});

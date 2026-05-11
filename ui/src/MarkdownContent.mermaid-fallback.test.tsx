import { cleanup, render, screen } from "@testing-library/react";
import mermaidBundleUrl from "mermaid/dist/mermaid.min.js?url";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MermaidModule } from "./mermaid-render";

type WindowWithMermaidBundle = typeof window & {
  mermaid?: MermaidModule;
  __termalMermaidBundleLoadPromise?: Promise<MermaidModule>;
};

describe("MarkdownContent Mermaid dynamic import fallback", () => {
  afterEach(() => {
    cleanup();
    vi.doUnmock("mermaid");
    vi.resetModules();
    vi.restoreAllMocks();
  });

  it("loads the bundled Mermaid script when the dynamic Mermaid import fails", async () => {
    vi.resetModules();
    vi.doMock("mermaid", () => ({
      get default() {
        throw new Error(
          "Failed to fetch dynamically imported module: http://127.0.0.1:4173/node_modules/.vite/deps/mermaid.js",
        );
      },
    }));

    const mermaidWindow = window as WindowWithMermaidBundle;
    const previousMermaid = mermaidWindow.mermaid;
    const previousBundleLoadPromise =
      mermaidWindow.__termalMermaidBundleLoadPromise;
    const fallbackRender = vi.fn(async (id: string) => ({
      diagramType: "flowchart",
      svg: `<svg data-testid="bundled-mermaid-svg" id="${id}"><text>fallback</text></svg>`,
    }));
    const fallbackMermaid = {
      initialize: vi.fn(),
      render: fallbackRender,
    } as unknown as MermaidModule;
    const appendChild = document.head.appendChild.bind(document.head);
    const expectedBundleSrc = new URL(
      mermaidBundleUrl,
      window.location.href,
    ).href;
    const appendedScripts: HTMLScriptElement[] = [];
    const appendChildSpy = vi
      .spyOn(document.head, "appendChild")
      .mockImplementation((node) => {
        const result = appendChild(node);
        if (node instanceof HTMLScriptElement) {
          appendedScripts.push(node);
          queueMicrotask(() => {
            mermaidWindow.mermaid = fallbackMermaid;
            node.onload?.(new Event("load"));
          });
        }
        return result;
      });

    delete mermaidWindow.mermaid;
    delete mermaidWindow.__termalMermaidBundleLoadPromise;

    try {
      const { MarkdownContent } = await import("./message-cards");

      render(
        <MarkdownContent
          markdown={["```mermaid", "flowchart TD", "  A --> B", "```"].join("\n")}
        />,
      );

      expect(await screen.findByTestId("mermaid-frame")).toBeInTheDocument();
      expect(appendChildSpy).toHaveBeenCalledWith(expect.any(HTMLScriptElement));
      expect(appendedScripts).toHaveLength(1);
      expect(appendedScripts[0]?.src).toBe(expectedBundleSrc);
      expect(fallbackRender).toHaveBeenCalledWith(
        expect.stringMatching(/^termal-mermaid-\d+$/),
        "flowchart TD\n  A --> B",
      );
      expect(screen.queryByText(/Mermaid render failed:/)).not.toBeInTheDocument();
    } finally {
      appendChildSpy.mockRestore();
      if (previousMermaid) {
        mermaidWindow.mermaid = previousMermaid;
      } else {
        delete mermaidWindow.mermaid;
      }
      if (previousBundleLoadPromise) {
        mermaidWindow.__termalMermaidBundleLoadPromise =
          previousBundleLoadPromise;
      } else {
        delete mermaidWindow.__termalMermaidBundleLoadPromise;
      }
    }
  });
});

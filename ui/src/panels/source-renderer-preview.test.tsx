import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { RendererPreviewPane } from "./source-renderer-preview";

describe("RendererPreviewPane", () => {
  it("keeps the read-only Markdown source branch covered for direct callers", () => {
    const { container } = render(
      <RendererPreviewPane
        appearance="light"
        content="# Rendered document"
        documentPath="/repo/docs/readme.md"
        isMarkdownSource
        renderableRegions={[]}
        workspaceRoot="/repo"
      />,
    );

    expect(
      screen.getByRole("heading", { name: "Rendered document" }),
    ).toBeInTheDocument();
    expect(container.querySelector(".markdown-document-view")).not.toBeNull();
  });
});

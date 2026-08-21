import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MarkdownContent } from "./markdown-content";
import { renderPlainTextWithSoftBreaks } from "./plain-text-wrapping";

const REPORTED_PATH =
  "supabase/migrations/20260821000000_restore_explicit_public_table_grants.sql";

describe("renderPlainTextWithSoftBreaks", () => {
  it("prefers path separators without changing copied text or splitting the extension", () => {
    const { container } = render(
      <p>{renderPlainTextWithSoftBreaks(REPORTED_PATH)}</p>,
    );
    const paragraph = container.querySelector("p");

    expect(paragraph).toHaveTextContent(REPORTED_PATH);
    expect(paragraph?.textContent).toBe(REPORTED_PATH);
    expect(paragraph?.querySelectorAll("wbr").length).toBe(7);
    expect(paragraph?.innerHTML).toContain("supabase/<wbr>migrations/<wbr>");
    expect(paragraph?.innerHTML).toContain("grants.sql");
    expect(paragraph?.innerHTML).not.toContain("grants.<wbr>sql");
  });

  it("keeps a search match highlighted across an inserted soft break", () => {
    const { container } = render(
      <p>
        {renderPlainTextWithSoftBreaks(REPORTED_PATH, "public_table", "active")}
      </p>,
    );
    const paragraph = container.querySelector("p");
    const highlight = paragraph?.querySelector("mark.search-highlight");

    expect(highlight).toHaveClass("is-active");
    expect(highlight?.textContent).toBe("public_table");
    expect(highlight?.querySelector("wbr")).not.toBeNull();
    expect(paragraph?.textContent).toBe(REPORTED_PATH);
  });

  it("does not add wrapping markup to ordinary short text", () => {
    const { container } = render(
      <p>{renderPlainTextWithSoftBreaks("czy to musi iść na produkcję?")}</p>,
    );

    expect(container.querySelector("wbr")).toBeNull();
  });

  it("adds the same clean break opportunities inside Markdown inline code", () => {
    const { container } = render(
      <MarkdownContent
        markdown={`\`${REPORTED_PATH}\``}
        searchQuery="public_table"
        searchHighlightTone="active"
      />,
    );
    const code = container.querySelector(".markdown-copy code");
    const highlight = code?.querySelector("mark.search-highlight");

    expect(code?.textContent).toBe(REPORTED_PATH);
    expect(code?.querySelectorAll("wbr").length).toBe(7);
    expect(code?.innerHTML).toContain("grants.sql");
    expect(code?.innerHTML).not.toContain("grants.<wbr>sql");
    expect(highlight).toHaveClass("is-active");
    expect(highlight?.textContent).toBe("public_table");
    expect(highlight?.querySelector("wbr")).not.toBeNull();
  });

  it("keeps arbitrary character breaks as a fallback instead of competing with wbr", async () => {
    const nodeFsModule = "node:fs";
    const { readFileSync } = (await import(nodeFsModule)) as {
      readFileSync: (path: string, encoding: "utf8") => string;
    };
    const runtimeProcess = (
      globalThis as typeof globalThis & {
        process: { cwd: () => string };
      }
    ).process;
    const styles = readFileSync(
      `${runtimeProcess.cwd()}/src/styles.css`,
      "utf8",
    );
    const plainTextRule = styles.match(/\.plain-text-copy\s*\{([^}]*)\}/)?.[1];
    const inlineCodeRule = styles.match(
      /\.markdown-copy\s+:not\(pre\)\s*>\s*code\s*\{([^}]*)\}/,
    )?.[1];
    const inlineCodeLinkRule = styles.match(
      /\.markdown-copy\s+a\.inline-code-link\s*\{([^}]*)\}/,
    )?.[1];

    for (const rule of [plainTextRule, inlineCodeRule, inlineCodeLinkRule]) {
      expect(rule).toMatch(/overflow-wrap:\s*break-word\s*;/);
      expect(rule).toMatch(/word-break:\s*normal\s*;/);
      expect(rule).not.toMatch(/overflow-wrap:\s*anywhere\s*;/);
    }
  });
});

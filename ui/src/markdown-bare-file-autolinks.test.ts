import { describe, expect, it } from "vitest";

import { remarkAutolinkBareFileReferences } from "./markdown-bare-file-autolinks";

type TestMarkdownAstNode = {
  type: string;
  value?: string;
  url?: string;
  children?: TestMarkdownAstNode[];
};

function runBareFileAutolinks(tree: TestMarkdownAstNode) {
  const plugin = remarkAutolinkBareFileReferences();
  plugin(tree);
  return tree;
}

function textNode(value: string): TestMarkdownAstNode {
  return { type: "text", value };
}

function linkUrls(node: TestMarkdownAstNode): string[] {
  const urls: string[] = [];
  const visit = (current: TestMarkdownAstNode) => {
    if (current.type === "link" && current.url) {
      urls.push(current.url);
    }
    current.children?.forEach(visit);
  };
  visit(node);
  return urls;
}

describe("remarkAutolinkBareFileReferences", () => {
  it("links multiple bare file references while preserving prefix text", () => {
    const tree = runBareFileAutolinks({
      type: "paragraph",
      children: [textNode("See src/lib.rs#L12 and docs/readme.md:4.")],
    });

    expect(linkUrls(tree)).toEqual(["src/lib.rs#L12", "docs/readme.md:4"]);
    expect(tree.children).toMatchObject([
      { type: "text", value: "See" },
      { type: "text", value: " " },
      {
        type: "link",
        url: "src/lib.rs#L12",
        children: [{ type: "text", value: "src/lib.rs#L12" }],
      },
      { type: "text", value: " and" },
      { type: "text", value: " " },
      {
        type: "link",
        url: "docs/readme.md:4",
        children: [{ type: "text", value: "docs/readme.md:4" }],
      },
      { type: "text", value: "." },
    ]);
  });

  it("links dotted line targets", () => {
    const tree = runBareFileAutolinks({
      type: "paragraph",
      children: [textNode("See foo.tex.#L63")],
    });

    expect(linkUrls(tree)).toEqual(["foo.tex.#L63"]);
  });

  it("ignores nodes whose contents should not be rewritten", () => {
    const ignoredTypes = [
      "code",
      "definition",
      "html",
      "inlineCode",
      "link",
      "linkReference",
    ];
    const tree = runBareFileAutolinks({
      type: "root",
      children: ignoredTypes.map((type) => ({
        type,
        value:
          type === "code" || type === "html" || type === "inlineCode"
            ? "src/lib.rs#L12"
            : undefined,
        children: [textNode("src/lib.rs#L12")],
      })),
    });

    expect(linkUrls(tree)).toEqual([]);
    expect(tree.children).toHaveLength(ignoredTypes.length);
    expect(tree.children?.map((child) => child.type)).toEqual(ignoredTypes);
  });

  it("resets global matching state across plugin calls", () => {
    const first = runBareFileAutolinks({
      type: "paragraph",
      children: [textNode("src/first.rs#L1")],
    });
    const second = runBareFileAutolinks({
      type: "paragraph",
      children: [textNode("src/second.rs#L2")],
    });

    expect(linkUrls(first)).toEqual(["src/first.rs#L1"]);
    expect(linkUrls(second)).toEqual(["src/second.rs#L2"]);
  });
});

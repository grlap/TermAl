// Owns Markdown AST rewriting for bare file references such as `src/lib.rs#L12`.
// Does not own link click handling, URL resolution, or Markdown rendering.
// Consumers must register this after remark-math and remark-gfm so those
// tokenizers claim math/table syntax before this plugin rewrites text nodes.
// Split from ui/src/message-cards.tsx.

type MarkdownAstNode = {
  type: string;
  value?: string;
  url?: string;
  children?: MarkdownAstNode[];
};

const MARKDOWN_BARE_FILE_REFERENCE_PATTERN =
  /(^|[\s([{"\'""`])((?:(?:\.\.?[\\/])|(?:\/[A-Za-z]:[\\/])|(?:[A-Za-z]:[\\/])|(?:\\\\)|\/)?(?:[\w.-]+[\\/])*[\w.-]+\.[A-Za-z0-9]{2,12}(?:\.?#L\d+(?:C\d+)?|:\d+(?::\d+)?))(?=$|[\s)\]},"\'"";!?`.])/g;
const MARKDOWN_BARE_FILE_REFERENCE_IGNORED_NODE_TYPES = new Set([
  "code",
  "definition",
  "html",
  "inlineCode",
  "link",
  "linkReference",
]);

export function remarkAutolinkBareFileReferences() {
  return (tree: MarkdownAstNode) => {
    autolinkMarkdownBareFileReferences(tree);
  };
}

function autolinkMarkdownBareFileReferences(node: MarkdownAstNode) {
  if (
    !Array.isArray(node.children) ||
    MARKDOWN_BARE_FILE_REFERENCE_IGNORED_NODE_TYPES.has(node.type)
  ) {
    return;
  }

  for (let index = 0; index < node.children.length; index += 1) {
    const child = node.children[index];
    if (child.type === "text" && typeof child.value === "string") {
      const replacement = buildAutolinkedMarkdownTextNodes(child.value);
      if (replacement) {
        node.children.splice(index, 1, ...replacement);
        index += replacement.length - 1;
        continue;
      }
    }

    autolinkMarkdownBareFileReferences(child);
  }
}

function buildAutolinkedMarkdownTextNodes(text: string) {
  MARKDOWN_BARE_FILE_REFERENCE_PATTERN.lastIndex = 0;
  const nodes: MarkdownAstNode[] = [];
  let changed = false;
  let lastIndex = 0;
  let match = MARKDOWN_BARE_FILE_REFERENCE_PATTERN.exec(text);

  while (match) {
    const prefix = match[1] ?? "";
    const reference = match[2] ?? "";
    const matchIndex = match.index;
    const referenceStartIndex = matchIndex + prefix.length;
    if (matchIndex > lastIndex) {
      nodes.push(createMarkdownTextNode(text.slice(lastIndex, matchIndex)));
    }
    if (prefix) {
      nodes.push(createMarkdownTextNode(prefix));
    }
    nodes.push(createMarkdownLinkNode(reference));
    lastIndex = referenceStartIndex + reference.length;
    changed = true;
    match = MARKDOWN_BARE_FILE_REFERENCE_PATTERN.exec(text);
  }

  if (!changed) {
    return null;
  }

  if (lastIndex < text.length) {
    nodes.push(createMarkdownTextNode(text.slice(lastIndex)));
  }

  return nodes.filter(
    (node) => node.type !== "text" || (node.value?.length ?? 0) > 0,
  );
}

function createMarkdownLinkNode(reference: string): MarkdownAstNode {
  return {
    type: "link",
    url: reference,
    children: [createMarkdownTextNode(reference)],
  };
}

function createMarkdownTextNode(value: string): MarkdownAstNode {
  return {
    type: "text",
    value,
  };
}

// Owns: syntax-highlighted code blocks and deferred activation for large blocks.
// Does not own: markdown AST rendering, command/diff cards, or Mermaid fallback policy.
// Split from: ui/src/message-cards.tsx.

import { useEffect, useMemo, useState } from "react";

import { copyTextToClipboard } from "./clipboard";
import { DeferredHeavyContent } from "./deferred-heavy-content";
import {
  buildDeferredPreviewText,
  estimateCodeBlockHeight,
  measureTextBlock,
} from "./deferred-render";
import { highlightCode } from "./highlight";
import { CheckIcon, CopyIcon } from "./message-card-icons";
import type { MarkdownLineAttributes } from "./markdown-line-markers";
import {
  containsSearchMatch,
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";

export function DeferredHighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
  preferImmediateRender = false,
  searchQuery,
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
  preferImmediateRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const metrics = useMemo(() => measureTextBlock(code), [code]);
  const showSearchHighlight = containsSearchMatch(code, searchQuery ?? "");
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_CODE_LINE_THRESHOLD ||
      code.length >= HEAVY_CODE_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    );
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateCodeBlockHeight(metrics.lineCount)}
      preferImmediateRender={preferImmediateRender}
      placeholder={
        <pre className={`${className} syntax-block deferred-code-placeholder`}>
          <code>{buildDeferredPreviewText(code)}</code>
        </pre>
      }
    >
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    </DeferredHeavyContent>
  );
}

export function HighlightedCodeBlock({
  className,
  code,
  commandHint,
  lineAttributes,
  language,
  pathHint,
  showCopyButton = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  lineAttributes?: MarkdownLineAttributes | null;
  language?: string | null;
  pathHint?: string | null;
  showCopyButton?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [copied, setCopied] = useState(false);
  const showSearchHighlight = containsSearchMatch(code, searchQuery);
  const highlighted = useMemo(
    () =>
      highlightCode(code, {
        commandHint,
        language,
        pathHint,
      }),
    [code, commandHint, language, pathHint],
  );
  const codeLanguage =
    highlighted.language ?? normalizeCodeLanguageClass(language);

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(code);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <pre
      {...(lineAttributes ?? {})}
      className={`${className} syntax-block${showCopyButton ? " copyable" : ""}`}
    >
      {showCopyButton ? (
        <button
          className={`command-icon-button syntax-copy-button${copied ? " copied" : ""}`}
          type="button"
          onMouseDown={(event) => {
            event.preventDefault();
            event.stopPropagation();
          }}
          onClick={() => void handleCopy()}
          aria-label={copied ? "Code copied" : "Copy code"}
          title={copied ? "Copied" : "Copy code"}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </button>
      ) : null}
      <code
        className={`hljs${codeLanguage ? ` language-${codeLanguage}` : ""}`}
      >
        {showSearchHighlight ? (
          renderHighlightedText(code, searchQuery, searchHighlightTone)
        ) : (
          <span dangerouslySetInnerHTML={{ __html: highlighted.html }} />
        )}
      </code>
    </pre>
  );
}

const HEAVY_CODE_CHARACTER_THRESHOLD = 1400;
const HEAVY_CODE_LINE_THRESHOLD = 28;

function normalizeCodeLanguageClass(language: string | null | undefined) {
  const normalized =
    language
      ?.trim()
      .toLowerCase()
      .replace(/^language-/, "") ?? "";
  return /^[a-z0-9_-]+$/.test(normalized) ? normalized : null;
}

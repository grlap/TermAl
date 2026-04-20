import { parseConnectionRetryNotice } from "./app-utils";
import type {
  ApprovalMessage,
  CommandMessage,
  DiffMessage,
  FileChangesMessage,
  ImageAttachment,
  MarkdownMessage,
  Message,
  ParallelAgentsMessage,
  PendingPrompt,
  SubagentResultMessage,
  Session,
  ThinkingMessage,
} from "./types";

export type SessionSearchItemKind = "message" | "pendingPrompt";

export type SessionSearchMatch = {
  itemId: string;
  itemKey: string;
  itemKind: SessionSearchItemKind;
  snippet: string;
};

export type SessionListSearchResult = {
  matchCount: number;
  snippet: string;
};

export type SessionSearchIndex = {
  items: SearchableSessionItem[];
  metadataParts: SearchableTextPart[];
  preview: string;
};

type SearchableTextPart = {
  text: string;
  normalizedText: string;
};

type SearchableSessionItem = {
  id: string;
  itemKey: string;
  kind: SessionSearchItemKind;
  text: string;
  normalizedText: string;
};

const SNIPPET_CONTEXT_CHARS = 48;

export function buildSessionSearchMatches(
  session: Session,
  rawQuery: string,
): SessionSearchMatch[] {
  return buildSessionSearchMatchesFromIndex(buildSessionSearchIndex(session), rawQuery);
}

export function buildSessionSearchMatchesFromIndex(
  searchIndex: SessionSearchIndex,
  rawQuery: string,
): SessionSearchMatch[] {
  const query = rawQuery.trim();
  if (!query) {
    return [];
  }

  const normalizedQuery = normalizeSearchValue(query);
  return searchIndex.items.flatMap((item) => {
    const matchIndex = item.normalizedText.indexOf(normalizedQuery);
    if (matchIndex < 0) {
      return [];
    }

    return [
      {
        itemId: item.id,
        itemKey: item.itemKey,
        itemKind: item.kind,
        snippet: buildSearchSnippet(item.text, matchIndex, query.length),
      },
    ];
  });
}

export function sessionSearchItemKey(kind: SessionSearchItemKind, itemId: string) {
  return `${kind}:${itemId}`;
}

export function buildSessionListSearchResult(
  session: Session,
  rawQuery: string,
): SessionListSearchResult | null {
  return buildSessionListSearchResultFromIndex(buildSessionSearchIndex(session), rawQuery);
}

export function buildSessionListSearchResultFromIndex(
  searchIndex: SessionSearchIndex,
  rawQuery: string,
): SessionListSearchResult | null {
  const query = rawQuery.trim();
  if (!query) {
    return null;
  }

  const metadataMatch = findSessionMetadataMatch(searchIndex, query);
  const conversationMatchSummary = summarizeConversationMatches(searchIndex, query);
  if (!metadataMatch && conversationMatchSummary.matchCount === 0) {
    return null;
  }

  return {
    matchCount: conversationMatchSummary.matchCount + (metadataMatch ? 1 : 0),
    snippet: conversationMatchSummary.firstSnippet ?? metadataMatch?.snippet ?? searchIndex.preview,
  };
}

export function buildSessionSearchIndex(session: Session): SessionSearchIndex {
  return {
    items: buildSearchableSessionItems(session),
    metadataParts: collectSessionMetadataSearchParts(session).map(createSearchableTextPart),
    preview: session.preview,
  };
}

function buildSearchableSessionItems(session: Session): SearchableSessionItem[] {
  return [
    ...session.messages.flatMap((message) =>
      createSearchableSessionItems(
        "message",
        message.id,
        collectMessageSearchText(message),
      ),
    ),
    ...(session.pendingPrompts ?? []).flatMap((prompt) =>
      createSearchableSessionItems(
        "pendingPrompt",
        prompt.id,
        collectPendingPromptSearchText(prompt),
      ),
    ),
  ];
}

function createSearchableSessionItems(
  kind: SessionSearchItemKind,
  id: string,
  text: string,
): SearchableSessionItem[] {
  if (!text) {
    return [];
  }

  const searchableText = createSearchableTextPart(text);
  return [
    {
      id,
      itemKey: sessionSearchItemKey(kind, id),
      kind,
      text: searchableText.text,
      normalizedText: searchableText.normalizedText,
    },
  ];
}

function createSearchableTextPart(text: string): SearchableTextPart {
  return {
    text,
    normalizedText: normalizeSearchValue(text),
  };
}

function collectMessageSearchText(message: Message) {
  switch (message.type) {
    case "text":
      // Connection-retry notices render as TWO distinct cards
      // depending on whether the retry is still live or the
      // turn has since resumed:
      //   - Live  → `notice.detail` (the stored `message.text`).
      //   - Resolved → a synthesized past-tense summary
      //     ("Connection dropped briefly; the turn continued
      //     after …"). The stored text is NOT rendered.
      // Indexing only `message.text` means a search for terms
      // that only appear in the resolved card ("Connection
      // recovered", "the turn continued") misses the match, and
      // a search for terms that only appear in the live card
      // ("Retrying automatically", "response finished") lands
      // on a resolved card whose visible text doesn't contain
      // the query — both look like broken search to the user.
      // Include BOTH forms so the index answers "does this
      // message match?" correctly regardless of which card
      // variant is currently rendered. Per-card highlight
      // positions are recomputed at render time against each
      // card's rendered text, so the card that IS visible
      // highlights correctly.
      return joinSearchableParts([
        collectAttachmentSearchText(message.attachments),
        message.text,
        ...collectConnectionRetrySearchText(message.text),
      ]);
    case "thinking":
      return collectThinkingSearchText(message);
    case "command":
      return collectCommandSearchText(message);
    case "diff":
      return collectDiffSearchText(message);
    case "markdown":
      return collectMarkdownSearchText(message);
    case "parallelAgents":
      return collectParallelAgentsSearchText(message);
    case "fileChanges":
      return collectFileChangesSearchText(message);
    case "subagentResult":
      return collectSubagentResultSearchText(message);
    case "approval":
      return collectApprovalSearchText(message);
    default:
      return "";
  }
}

function collectThinkingSearchText(message: ThinkingMessage) {
  return joinSearchableParts([message.title, ...message.lines]);
}

function collectCommandSearchText(message: CommandMessage) {
  return joinSearchableParts([message.command, message.output]);
}

function collectDiffSearchText(message: DiffMessage) {
  return joinSearchableParts([message.filePath, message.summary, message.diff]);
}

function collectMarkdownSearchText(message: MarkdownMessage) {
  return joinSearchableParts([message.title, message.markdown]);
}

function collectParallelAgentsSearchText(message: ParallelAgentsMessage) {
  return joinSearchableParts(
    message.agents.flatMap((agent) => [agent.title, agent.detail]),
  );
}
function collectFileChangesSearchText(message: FileChangesMessage) {
  return joinSearchableParts([
    message.title,
    ...message.files.flatMap((file) => [file.path, file.kind]),
  ]);
}
function collectSubagentResultSearchText(message: SubagentResultMessage) {
  return joinSearchableParts([message.title, message.summary, message.conversationId, message.turnId]);
}

function collectApprovalSearchText(message: ApprovalMessage) {
  return joinSearchableParts([message.title, message.command, message.detail]);
}

function collectPendingPromptSearchText(prompt: PendingPrompt) {
  return joinSearchableParts([
    collectAttachmentSearchText(prompt.attachments),
    prompt.text,
  ]);
}

function collectAttachmentSearchText(attachments: ImageAttachment[] | undefined) {
  return joinSearchableParts(
    (attachments ?? []).flatMap((attachment) => [attachment.fileName, attachment.mediaType]),
  );
}

// Returns the searchable text fragments a connection-retry
// notice RENDERS into when the turn has resumed (the resolved
// `ConnectionRetryCard` variant in `message-cards.tsx`). Empty
// when the message text does not parse as a retry notice. The
// strings here mirror the literals in
// `message-cards.tsx::ConnectionRetryCard` — keep them in sync
// with the rendered copy so search-over-resolved-cards stays
// correct.
function collectConnectionRetrySearchText(text: string): string[] {
  const notice = parseConnectionRetryNotice(text);
  if (!notice) {
    return [];
  }
  const resolvedHeading = "Connection recovered";
  const resolvedDetail = notice.attemptLabel
    ? `Connection dropped briefly; the turn continued after ${notice.attemptLabel.toLowerCase()}.`
    : "Connection dropped briefly; the turn continued after an automatic retry.";
  return [resolvedHeading, resolvedDetail];
}

function joinSearchableParts(parts: Array<string | null | undefined>) {
  return parts
    .map((part) => part?.trim())
    .filter((part): part is string => Boolean(part))
    .join("\n");
}

function findSessionMetadataMatch(searchIndex: SessionSearchIndex, query: string) {
  const normalizedQuery = normalizeSearchValue(query);
  return searchIndex.metadataParts.flatMap((part) => {
    const matchIndex = part.normalizedText.indexOf(normalizedQuery);
    if (matchIndex < 0) {
      return [];
    }

    return [{ snippet: buildSearchSnippet(part.text, matchIndex, query.length) }];
  })[0] ?? null;
}

function summarizeConversationMatches(searchIndex: SessionSearchIndex, query: string) {
  const normalizedQuery = normalizeSearchValue(query);
  let matchCount = 0;
  let firstSnippet: string | null = null;

  for (const item of searchIndex.items) {
    const matchIndex = item.normalizedText.indexOf(normalizedQuery);
    if (matchIndex < 0) {
      continue;
    }

    matchCount += 1;
    if (firstSnippet === null) {
      firstSnippet = buildSearchSnippet(item.text, matchIndex, query.length);
    }
  }

  return { matchCount, firstSnippet };
}

function collectSessionMetadataSearchParts(session: Session) {
  return [
    session.name,
    session.workdir,
    session.preview,
    session.agent,
    session.model,
  ].filter((part): part is string => Boolean(part?.trim()));
}

function buildSearchSnippet(text: string, startIndex: number, queryLength: number) {
  const snippetStart = Math.max(startIndex - SNIPPET_CONTEXT_CHARS, 0);
  const snippetEnd = Math.min(text.length, startIndex + queryLength + SNIPPET_CONTEXT_CHARS);
  const snippet = text
    .slice(snippetStart, snippetEnd)
    .replace(/\s+/g, " ")
    .trim();

  const prefix = snippetStart > 0 ? "…" : "";
  const suffix = snippetEnd < text.length ? "…" : "";
  return `${prefix}${snippet}${suffix}`;
}

function normalizeSearchValue(value: string) {
  return value.toLocaleLowerCase();
}

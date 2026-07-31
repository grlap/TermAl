// Pure presentation rules for durable mailbox cards and workspace threads.
// Server-side cursor state remains authoritative; human viewing is read-only.

import type {
  MailboxMessage,
  MailboxParticipant,
  MailboxSummary,
} from "./types";

export const MAILBOX_PAGE_SIZE = 50;

export function mailboxAgentTone(
  displayName: string,
): "fable" | "codex" | "neutral" {
  const normalized = displayName.toLowerCase();
  if (normalized.includes("fable")) {
    return "fable";
  }
  if (normalized.includes("codex")) {
    return "codex";
  }
  return "neutral";
}

export function firstSentence(value: string | null | undefined): string {
  const normalized = value?.replace(/\s+/g, " ").trim() ?? "";
  if (!normalized) {
    return "";
  }

  const boundary = normalized.search(/[.!?](?:\s|$)/);
  return boundary >= 0 ? normalized.slice(0, boundary + 1) : normalized;
}

export function mailboxMessageTopic(message: MailboxMessage): string {
  return (
    message.topic?.trim() ||
    firstSentence(message.body) ||
    `Message #${message.sequence}`
  );
}

export function mailboxTargetCursor(
  message: MailboxMessage,
  participants: readonly MailboxParticipant[],
): number {
  return (
    participants.find(
      (participant) => participant.sessionId === message.targetSessionId,
    )?.processedThrough ?? 0
  );
}

export function mailboxMessageIsProcessed(
  message: MailboxMessage,
  participants: readonly MailboxParticipant[],
): boolean {
  return message.sequence <= mailboxTargetCursor(message, participants);
}

export function laggingMailboxParticipants(
  summary: MailboxSummary,
): MailboxParticipant[] {
  return summary.participants.filter(
    (participant) => participant.processedThrough < summary.latestSequence,
  );
}

export function mailboxCursorDividerIndex(
  messagesNewestFirst: readonly MailboxMessage[],
  processedThrough: number,
): number | null {
  if (messagesNewestFirst.length === 0) {
    return null;
  }

  const index = messagesNewestFirst.findIndex(
    (message) => message.sequence <= processedThrough,
  );
  return index >= 0 ? index : null;
}

export function mergeMailboxMessagesNewestFirst(
  current: readonly MailboxMessage[],
  next: readonly MailboxMessage[],
): MailboxMessage[] {
  const byId = new Map<string, MailboxMessage>();
  for (const message of [...current, ...next]) {
    byId.set(message.id, message);
  }
  return Array.from(byId.values()).sort(
    (left, right) => right.sequence - left.sequence,
  );
}

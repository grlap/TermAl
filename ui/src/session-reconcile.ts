import type {
  ApprovalMessage,
  CommandMessage,
  DiffMessage,
  ImageAttachment,
  MarkdownMessage,
  Message,
  PendingPrompt,
  Session,
  TextMessage,
  ThinkingMessage,
} from "./types";

export function reconcileSessions(previous: Session[], next: Session[]): Session[] {
  const previousById = new Map(previous.map((session) => [session.id, session]));
  let changed = previous.length !== next.length;

  const merged = next.map((nextSession, index) => {
    const previousSession = previousById.get(nextSession.id);
    if (!previousSession) {
      changed = true;
      return nextSession;
    }

    const mergedSession = reconcileSession(previousSession, nextSession);
    if (mergedSession !== previousSession || previous[index]?.id !== nextSession.id) {
      changed = true;
    }
    return mergedSession;
  });

  return changed ? merged : previous;
}

function reconcileSession(previous: Session, next: Session): Session {
  const messages = reconcileMessages(previous.messages, next.messages);
  const pendingPrompts = reconcilePendingPrompts(previous.pendingPrompts, next.pendingPrompts);

  if (
    previous.name === next.name &&
    previous.emoji === next.emoji &&
    previous.agent === next.agent &&
    previous.workdir === next.workdir &&
    previous.model === next.model &&
    sameModelOptions(previous.modelOptions, next.modelOptions) &&
    previous.approvalPolicy === next.approvalPolicy &&
    previous.claudeEffort === next.claudeEffort &&
    previous.reasoningEffort === next.reasoningEffort &&
    previous.sandboxMode === next.sandboxMode &&
    previous.cursorMode === next.cursorMode &&
    previous.claudeApprovalMode === next.claudeApprovalMode &&
    previous.geminiApprovalMode === next.geminiApprovalMode &&
    previous.externalSessionId === next.externalSessionId &&
    previous.status === next.status &&
    previous.preview === next.preview &&
    messages === previous.messages &&
    pendingPrompts === previous.pendingPrompts
  ) {
    return previous;
  }

  if (pendingPrompts) {
    return {
      ...next,
      messages,
      pendingPrompts,
    };
  }

  const { pendingPrompts: _discard, ...rest } = next;
  return {
    ...rest,
    messages,
  };
}

function sameModelOptions(previous?: Session["modelOptions"], next?: Session["modelOptions"]) {
  if (previous === next) {
    return true;
  }
  if (!previous?.length && !next?.length) {
    return true;
  }
  if (!previous || !next || previous.length !== next.length) {
    return false;
  }

  return previous.every((option, index) => {
    const nextOption = next[index];
    const previousBadges = option.badges ?? [];
    const nextBadges = nextOption?.badges ?? [];
    const previousSupportedClaudeEfforts = option.supportedClaudeEffortLevels ?? [];
    const nextSupportedClaudeEfforts = nextOption?.supportedClaudeEffortLevels ?? [];
    const previousSupportedReasoningEfforts = option.supportedReasoningEfforts ?? [];
    const nextSupportedReasoningEfforts = nextOption?.supportedReasoningEfforts ?? [];
    return (
      nextOption?.label === option.label &&
      nextOption.value === option.value &&
      (nextOption.description ?? null) === (option.description ?? null) &&
      (nextOption.defaultReasoningEffort ?? null) === (option.defaultReasoningEffort ?? null) &&
      previousBadges.length === nextBadges.length &&
      previousBadges.every((badge, badgeIndex) => nextBadges[badgeIndex] === badge) &&
      previousSupportedClaudeEfforts.length === nextSupportedClaudeEfforts.length &&
      previousSupportedClaudeEfforts.every(
        (effort, effortIndex) => nextSupportedClaudeEfforts[effortIndex] === effort,
      ) &&
      previousSupportedReasoningEfforts.length === nextSupportedReasoningEfforts.length &&
      previousSupportedReasoningEfforts.every(
        (effort, effortIndex) => nextSupportedReasoningEfforts[effortIndex] === effort,
      )
    );
  });
}

function reconcileMessages(previous: Message[], next: Message[]): Message[] {
  const previousById = new Map(previous.map((message) => [message.id, message]));
  let changed = previous.length !== next.length;

  const merged = next.map((nextMessage, index) => {
    const previousMessage =
      previous[index]?.id === nextMessage.id ? previous[index] : previousById.get(nextMessage.id);
    if (!previousMessage) {
      changed = true;
      return nextMessage;
    }

    const mergedMessage = reconcileMessage(previousMessage, nextMessage);
    if (mergedMessage !== previousMessage || previous[index]?.id !== nextMessage.id) {
      changed = true;
    }
    return mergedMessage;
  });

  return changed ? merged : previous;
}

function reconcileMessage(previous: Message, next: Message): Message {
  if (previous.type !== next.type) {
    return next;
  }

  switch (next.type) {
    case "text":
      return reconcileTextMessage(previous as TextMessage, next);
    case "thinking":
      return reconcileThinkingMessage(previous as ThinkingMessage, next);
    case "command":
      return reconcileCommandMessage(previous as CommandMessage, next);
    case "diff":
      return reconcileDiffMessage(previous as DiffMessage, next);
    case "markdown":
      return reconcileMarkdownMessage(previous as MarkdownMessage, next);
    case "approval":
      return reconcileApprovalMessage(previous as ApprovalMessage, next);
  }
}

function reconcileTextMessage(previous: TextMessage, next: TextMessage): TextMessage {
  const attachments = reconcileAttachments(previous.attachments, next.attachments);
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.text === next.text &&
    attachments === previous.attachments
  ) {
    return previous;
  }

  if (attachments) {
    return {
      ...next,
      attachments,
    };
  }

  const { attachments: _discard, ...rest } = next;
  return rest;
}

function reconcileThinkingMessage(previous: ThinkingMessage, next: ThinkingMessage): ThinkingMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    stringArrayEqual(previous.lines, next.lines)
  ) {
    return previous;
  }

  return next;
}

function reconcileCommandMessage(previous: CommandMessage, next: CommandMessage): CommandMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.command === next.command &&
    previous.commandLanguage === next.commandLanguage &&
    previous.output === next.output &&
    previous.outputLanguage === next.outputLanguage &&
    previous.status === next.status
  ) {
    return previous;
  }

  return next;
}

function reconcileDiffMessage(previous: DiffMessage, next: DiffMessage): DiffMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.filePath === next.filePath &&
    previous.summary === next.summary &&
    previous.diff === next.diff &&
    previous.language === next.language &&
    previous.changeType === next.changeType
  ) {
    return previous;
  }

  return next;
}

function reconcileMarkdownMessage(previous: MarkdownMessage, next: MarkdownMessage): MarkdownMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.markdown === next.markdown
  ) {
    return previous;
  }

  return next;
}

function reconcileApprovalMessage(previous: ApprovalMessage, next: ApprovalMessage): ApprovalMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.command === next.command &&
    previous.commandLanguage === next.commandLanguage &&
    previous.detail === next.detail &&
    previous.decision === next.decision
  ) {
    return previous;
  }

  return next;
}

function reconcilePendingPrompts(
  previous: PendingPrompt[] | undefined,
  next: PendingPrompt[] | undefined,
): PendingPrompt[] | undefined {
  if (!next?.length) {
    return undefined;
  }

  if (!previous?.length) {
    return next;
  }

  const previousById = new Map(previous.map((prompt) => [prompt.id, prompt]));
  let changed = previous.length !== next.length;

  const merged = next.map((nextPrompt, index) => {
    const previousPrompt =
      previous[index]?.id === nextPrompt.id ? previous[index] : previousById.get(nextPrompt.id);
    if (!previousPrompt) {
      changed = true;
      return nextPrompt;
    }

    const attachments = reconcileAttachments(previousPrompt.attachments, nextPrompt.attachments);
    if (
      previousPrompt.timestamp === nextPrompt.timestamp &&
      previousPrompt.text === nextPrompt.text &&
      attachments === previousPrompt.attachments
    ) {
      if (previous[index]?.id !== nextPrompt.id) {
        changed = true;
      }
      return previousPrompt;
    }

    changed = true;
    if (attachments) {
      return {
        ...nextPrompt,
        attachments,
      };
    }

    const { attachments: _discard, ...rest } = nextPrompt;
    return rest;
  });

  return changed ? merged : previous;
}

function reconcileAttachments(
  previous: ImageAttachment[] | undefined,
  next: ImageAttachment[] | undefined,
): ImageAttachment[] | undefined {
  if (!next?.length) {
    return undefined;
  }

  if (!previous?.length) {
    return next;
  }

  if (previous.length !== next.length) {
    return next;
  }

  for (let index = 0; index < next.length; index += 1) {
    const previousAttachment = previous[index];
    const nextAttachment = next[index];
    if (
      previousAttachment.fileName !== nextAttachment.fileName ||
      previousAttachment.mediaType !== nextAttachment.mediaType ||
      previousAttachment.byteSize !== nextAttachment.byteSize
    ) {
      return next;
    }
  }

  return previous;
}

function stringArrayEqual(previous: string[], next: string[]) {
  if (previous.length !== next.length) {
    return false;
  }

  for (let index = 0; index < previous.length; index += 1) {
    if (previous[index] !== next[index]) {
      return false;
    }
  }

  return true;
}

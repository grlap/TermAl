import type {
  ApprovalMessage,
  CodexAppRequestMessage,
  CommandMessage,
  DiffMessage,
  FileChangesMessage,
  ImageAttachment,
  MarkdownMessage,
  Message,
  McpElicitationRequestMessage,
  McpElicitationRequestPayload,
  ParallelAgentsMessage,
  PendingPrompt,
  Session,
  SubagentResultMessage,
  TextMessage,
  ThinkingMessage,
  UserInputQuestion,
  UserInputRequestMessage,
} from "./types";

type ReconcileSessionsOptions = {
  disableMutationStampFastPath?: boolean;
};

export function reconcileSessions(
  previous: Session[],
  next: Session[],
  options?: ReconcileSessionsOptions,
) {
  let previousById: Map<string, Session> | null = null;
  let changed = previous.length !== next.length;

  const merged = next.map((nextSession, index) => {
    const previousSession =
      previous[index]?.id === nextSession.id
        ? previous[index]
        : (previousById ??= new Map(
            previous.map((session) => [session.id, session]),
          )).get(nextSession.id);
    if (!previousSession) {
      changed = true;
      return nextSession;
    }

    const mergedSession = reconcileSession(
      previousSession,
      nextSession,
      options,
    );
    if (mergedSession !== previousSession || previous[index]?.id !== nextSession.id) {
      changed = true;
    }
    return mergedSession;
  });

  return changed ? merged : previous;
}

function sameSessionSummary(previous: Session, next: Session) {
  return (
    previous.name === next.name &&
    previous.emoji === next.emoji &&
    previous.agent === next.agent &&
    previous.workdir === next.workdir &&
    previous.projectId === next.projectId &&
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
    previous.agentCommandsRevision === next.agentCommandsRevision &&
    previous.codexThreadState === next.codexThreadState &&
    previous.sessionMutationStamp === next.sessionMutationStamp &&
    previous.status === next.status &&
    previous.preview === next.preview
  );
}

function reconcileSession(
  previous: Session,
  next: Session,
  options?: ReconcileSessionsOptions,
): Session {
  if (
    !options?.disableMutationStampFastPath &&
    previous.sessionMutationStamp !== null &&
    previous.sessionMutationStamp !== undefined &&
    previous.sessionMutationStamp === next.sessionMutationStamp
  ) {
    return previous;
  }
  const messages = reconcileMessages(previous.messages, next.messages);
  const pendingPrompts = reconcilePendingPrompts(previous.pendingPrompts, next.pendingPrompts);

  if (sameSessionSummary(previous, next) && messages === previous.messages && pendingPrompts === previous.pendingPrompts) {
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
  if (previous === next) {
    return previous;
  }

  if (previous.length === next.length && previous.length > 0) {
    const previousLastMessage = previous[previous.length - 1];
    const nextLastMessage = next[next.length - 1];
    if (
      previousLastMessage &&
      nextLastMessage &&
      previousLastMessage.id === nextLastMessage.id
    ) {
      let firstChangedIndex = -1;
      for (let index = 0; index < next.length; index += 1) {
        if (previous[index]?.id !== next[index]?.id) {
          firstChangedIndex = index;
          break;
        }
        if (reconcileMessage(previous[index]!, next[index]!) !== previous[index]) {
          firstChangedIndex = index;
          break;
        }
      }

      if (firstChangedIndex === -1) {
        return previous;
      }

      const merged = previous.slice(0, firstChangedIndex);
      for (let index = firstChangedIndex; index < next.length; index += 1) {
        const previousMessage = previous[index];
        const nextMessage = next[index];
        if (previousMessage?.id === nextMessage?.id) {
          merged.push(reconcileMessage(previousMessage, nextMessage));
          continue;
        }
        return reconcileMessagesById(previous, next);
      }
      return merged;
    }
  }

  return reconcileMessagesById(previous, next);
}

function reconcileMessagesById(previous: Message[], next: Message[]): Message[] {
  let previousById: Map<string, Message> | null = null;
  let changed = previous.length !== next.length;

  const merged = next.map((nextMessage, index) => {
    const previousMessage =
      previous[index]?.id === nextMessage.id
        ? previous[index]
        : (previousById ??= new Map(
            previous.map((message) => [message.id, message]),
          )).get(nextMessage.id);
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
    case "parallelAgents":
      return reconcileParallelAgentsMessage(previous as ParallelAgentsMessage, next);
    case "fileChanges":
      return reconcileFileChangesMessage(previous as FileChangesMessage, next);
    case "subagentResult":
      return reconcileSubagentResultMessage(previous as SubagentResultMessage, next);
    case "approval":
      return reconcileApprovalMessage(previous as ApprovalMessage, next);
    case "userInputRequest":
      return reconcileUserInputRequestMessage(previous as UserInputRequestMessage, next);
    case "mcpElicitationRequest":
      return reconcileMcpElicitationRequestMessage(
        previous as McpElicitationRequestMessage,
        next,
      );
    case "codexAppRequest":
      return reconcileCodexAppRequestMessage(previous as CodexAppRequestMessage, next);
  }

  return next;
}

function reconcileTextMessage(previous: TextMessage, next: TextMessage): TextMessage {
  const attachments = reconcileAttachments(previous.attachments, next.attachments);
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.text === next.text &&
    (previous.expandedText ?? null) === (next.expandedText ?? null) &&
    attachments === previous.attachments
  ) {
    return previous;
  }

  if (attachments === next.attachments) {
    return next;
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
    previous.changeSetId === next.changeSetId &&
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

function reconcileParallelAgentsMessage(
  previous: ParallelAgentsMessage,
  next: ParallelAgentsMessage,
): ParallelAgentsMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.agents.length === next.agents.length &&
    previous.agents.every((agent, index) => {
      const nextAgent = next.agents[index];
      return (
        nextAgent?.id === agent.id &&
        nextAgent.title === agent.title &&
        nextAgent.status === agent.status &&
        (nextAgent.detail ?? null) === (agent.detail ?? null)
      );
    })
  ) {
    return previous;
  }

  return next;
}
function reconcileFileChangesMessage(
  previous: FileChangesMessage,
  next: FileChangesMessage,
): FileChangesMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.files.length === next.files.length &&
    previous.files.every((file, index) => {
      const nextFile = next.files[index];
      return nextFile?.path === file.path && nextFile.kind === file.kind;
    })
  ) {
    return previous;
  }

  return next;
}
function reconcileSubagentResultMessage(
  previous: SubagentResultMessage,
  next: SubagentResultMessage,
): SubagentResultMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.summary === next.summary &&
    previous.conversationId === next.conversationId &&
    previous.turnId === next.turnId
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

function reconcileUserInputRequestMessage(
  previous: UserInputRequestMessage,
  next: UserInputRequestMessage,
): UserInputRequestMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.detail === next.detail &&
    previous.state === next.state &&
    sameUserInputQuestions(previous.questions, next.questions) &&
    sameSubmittedAnswers(previous.submittedAnswers, next.submittedAnswers)
  ) {
    return previous;
  }

  return next;
}

function reconcileMcpElicitationRequestMessage(
  previous: McpElicitationRequestMessage,
  next: McpElicitationRequestMessage,
): McpElicitationRequestMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.detail === next.detail &&
    previous.state === next.state &&
    previous.submittedAction === next.submittedAction &&
    sameMcpElicitationRequest(previous.request, next.request) &&
    sameJsonValue(previous.submittedContent, next.submittedContent)
  ) {
    return previous;
  }

  return next;
}

function reconcileCodexAppRequestMessage(
  previous: CodexAppRequestMessage,
  next: CodexAppRequestMessage,
): CodexAppRequestMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.detail === next.detail &&
    previous.method === next.method &&
    previous.state === next.state &&
    sameJsonValue(previous.params, next.params) &&
    sameJsonValue(previous.submittedResult, next.submittedResult)
  ) {
    return previous;
  }

  return next;
}

function sameUserInputQuestions(previous: UserInputQuestion[], next: UserInputQuestion[]) {
  return (
    previous.length === next.length &&
    previous.every((question, index) => {
      const nextQuestion = next[index];
      if (!nextQuestion) {
        return false;
      }

      const previousOptions = question.options ?? [];
      const nextOptions = nextQuestion.options ?? [];
      return (
        question.header === nextQuestion.header &&
        question.id === nextQuestion.id &&
        question.isOther === nextQuestion.isOther &&
        question.isSecret === nextQuestion.isSecret &&
        question.question === nextQuestion.question &&
        previousOptions.length === nextOptions.length &&
        previousOptions.every(
          (option, optionIndex) =>
            option.label === nextOptions[optionIndex]?.label &&
            option.description === nextOptions[optionIndex]?.description,
        )
      );
    })
  );
}

function sameSubmittedAnswers(
  previous?: UserInputRequestMessage["submittedAnswers"],
  next?: UserInputRequestMessage["submittedAnswers"],
) {
  const previousEntries = Object.entries(previous ?? {});
  const nextEntries = Object.entries(next ?? {});
  return (
    previousEntries.length === nextEntries.length &&
    previousEntries.every(([key, value]) => {
      const nextValue = next?.[key];
      return (
        !!nextValue &&
        value.length === nextValue.length &&
        value.every((entry, index) => entry === nextValue[index])
      );
    })
  );
}

function sameMcpElicitationRequest(
  previous: McpElicitationRequestPayload,
  next: McpElicitationRequestPayload,
) {
  return sameJsonValue(previous, next);
}

function sameJsonValue(previous: unknown, next: unknown): boolean {
  if (previous === next) {
    return true;
  }
  if (previous == null || next == null) {
    return (previous ?? null) === (next ?? null);
  }
  if (typeof previous !== typeof next) {
    return false;
  }
  if (typeof previous !== "object") {
    return previous === next;
  }
  if (Array.isArray(previous)) {
    if (!Array.isArray(next) || previous.length !== next.length) {
      return false;
    }
    return previous.every((item, index) => sameJsonValue(item, next[index]));
  }
  if (Array.isArray(next)) {
    return false;
  }
  const previousObj = previous as Record<string, unknown>;
  const nextObj = next as Record<string, unknown>;
  const previousKeys = Object.keys(previousObj);
  const nextKeys = Object.keys(nextObj);
  if (previousKeys.length !== nextKeys.length) {
    return false;
  }
  return previousKeys.every(
    (key) => key in nextObj && sameJsonValue(previousObj[key], nextObj[key]),
  );
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

  let previousById: Map<string, PendingPrompt> | null = null;
  let changed = previous.length !== next.length;

  const merged = next.map((nextPrompt, index) => {
    const previousPrompt =
      previous[index]?.id === nextPrompt.id
        ? previous[index]
        : (previousById ??= new Map(
            previous.map((prompt) => [prompt.id, prompt]),
          )).get(nextPrompt.id);
    if (!previousPrompt) {
      changed = true;
      return nextPrompt;
    }

    const attachments = reconcileAttachments(previousPrompt.attachments, nextPrompt.attachments);
    if (
      previousPrompt.timestamp === nextPrompt.timestamp &&
      previousPrompt.text === nextPrompt.text &&
      (previousPrompt.expandedText ?? null) === (nextPrompt.expandedText ?? null) &&
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

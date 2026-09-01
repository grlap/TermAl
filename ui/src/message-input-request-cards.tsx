// Owns interactive request cards rendered by the message-card switchboard.
// Deliberately does not own generic MessageCard routing or Markdown rendering;
// this was split out of `message-cards.tsx` as a pure code move.

import { useEffect, useId, useRef, useState } from "react";
import { MessageMeta } from "./message-card-meta";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type {
  CodexAppRequestMessage,
  JsonValue,
  McpElicitationAction,
  McpElicitationPrimitiveSchema,
  McpElicitationRequestMessage,
  UserInputQuestion,
  UserInputRequestMessage,
} from "./types";

type UserInputDraftField = {
  customAnswer: string;
  otherSelected: boolean;
  selectedOptions: string[];
};

type McpElicitationDraftField = {
  selectedOption: string;
  selections: string[];
  text: string;
};

function buildUserInputDraft(
  questions: UserInputQuestion[],
  submittedAnswers?: Record<string, string[]> | null,
): Record<string, UserInputDraftField> {
  const next: Record<string, UserInputDraftField> = {};
  for (const question of questions) {
    const answers = submittedAnswers?.[question.id] ?? [];
    const optionLabels = new Set(
      (question.options ?? []).map((option) => option.label),
    );
    const selectedOptions = answers.filter((answer) => optionLabels.has(answer));
    const customAnswers = answers.filter((answer) => !optionLabels.has(answer));
    next[question.id] = {
      customAnswer: customAnswers.includes("[secret provided]")
        ? ""
        : customAnswers.join(", "),
      otherSelected: !!question.isOther && customAnswers.length > 0,
      selectedOptions,
    };
  }
  return next;
}

function buildUserInputSummary(
  message: UserInputRequestMessage,
  searchQuery: string,
  searchHighlightTone: SearchHighlightTone,
) {
  const submittedAnswers = message.submittedAnswers ?? {};
  return message.questions
    .filter((question) => submittedAnswers[question.id]?.length)
    .map((question) => (
      <div key={question.id} className="user-input-summary-row">
        <div className="user-input-summary-header">
          {renderHighlightedText(
            question.header,
            searchQuery,
            searchHighlightTone,
          )}
        </div>
        <div className="user-input-summary-value">
          {renderHighlightedText(
            submittedAnswers[question.id]!.join(", "),
            searchQuery,
            searchHighlightTone,
          )}
        </div>
      </div>
    ));
}

export function UserInputRequestCard({
  message,
  onSubmit,
  actionsEnabled = true,
  submissionPending = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: UserInputRequestMessage;
  onSubmit: (
    messageId: string,
    answers: Record<string, string[]> | null,
  ) => void | Promise<unknown>;
  /** When false (board snapshots, staging trays), Submit/Skip render
   * disabled so a stale copy of the card cannot look actionable. */
  actionsEnabled?: boolean;
  /** Stable virtualizer-owned claim for a request already in flight. */
  submissionPending?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [draft, setDraft] = useState<Record<string, UserInputDraftField>>(() =>
    buildUserInputDraft(message.questions, message.submittedAnswers),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  // In-flight guard: set as soon as Submit or Skip dispatches so a
  // double-click cannot fire a second request (and a second error toast).
  // Cleared when the submission settles or the message itself updates.
  const [dispatched, setDispatched] = useState(false);
  // Unmount and stale-settle guards for the dispatch below: a submission
  // that settles after unmount (or after a newer dispatch superseded it)
  // must not call setState.
  const mountedRef = useRef(true);
  const dispatchedRef = useRef(false);
  const dispatchGenerationRef = useRef(0);
  // Radio groups are keyed per rendered instance, not per message: a live
  // card and a board snapshot of the same message would otherwise form one
  // native radio group and steal each other's selection.
  const instanceId = useId();
  const pending = message.state === "pending";
  // The whole card freezes while a submission is in flight, not only the
  // Submit/Skip buttons: editing the draft mid-flight would desync what the
  // user sees from what was sent.
  const interactive =
    pending && actionsEnabled && !dispatched && !submissionPending;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // The in-flight guard is owned by dispatch completion (generation-checked
  // settle). Ordinary message updates while a submission is in flight must
  // not lift it; only a genuinely different message resets it.
  useEffect(() => {
    dispatchGenerationRef.current += 1;
    dispatchedRef.current = false;
    setDispatched(false);
  }, [message.id]);

  useEffect(() => {
    // Preserve the visible draft when a pending card is re-synced while its
    // submission is still in flight. If the message resolves, server-owned
    // submitted answers replace the draft immediately.
    if (dispatchedRef.current && message.state === "pending") {
      return;
    }
    setDraft(buildUserInputDraft(message.questions, message.submittedAnswers));
    setValidationError(null);
  }, [message.id, message.questions, message.state, message.submittedAnswers]);

  function dispatchSubmission(answers: Record<string, string[]> | null) {
    if (dispatchedRef.current || submissionPending) {
      return;
    }
    const generation = dispatchGenerationRef.current + 1;
    dispatchGenerationRef.current = generation;
    dispatchedRef.current = true;
    setDispatched(true);
    const settle = () => {
      if (mountedRef.current && dispatchGenerationRef.current === generation) {
        dispatchedRef.current = false;
        setDispatched(false);
      }
    };
    let result: void | Promise<unknown>;
    try {
      result = onSubmit(message.id, answers);
    } catch {
      // Error reporting belongs to the app-level handler; a synchronous
      // throw here only re-enables the actions so the user can retry.
      settle();
      return;
    }
    // `.then(settle, settle)` observes rejections (no unhandled-rejection
    // event) while re-enabling the actions on either outcome; the app
    // handler already reported any failure to the user.
    void Promise.resolve(result).then(settle, settle);
  }

  function updateField(
    questionId: string,
    nextField: Partial<UserInputDraftField>,
  ) {
    setDraft((current) => ({
      ...current,
      [questionId]: {
        customAnswer: current[questionId]?.customAnswer ?? "",
        otherSelected: current[questionId]?.otherSelected ?? false,
        selectedOptions: current[questionId]?.selectedOptions ?? [],
        ...nextField,
      },
    }));
  }

  function handleSubmit() {
    const answers: Record<string, string[]> = {};
    for (const question of message.questions) {
      const field = draft[question.id] ?? {
        customAnswer: "",
        otherSelected: false,
        selectedOptions: [],
      };
      const options = question.options ?? [];
      const optionLabels = new Set(
        options.map((option) => option.label),
      );
      const selectedAnswers = [...field.selectedOptions];
      const customAnswer = field.customAnswer.trim();
      if ((options.length === 0 || field.otherSelected) && customAnswer) {
        selectedAnswers.push(customAnswer);
      }

      if (
        selectedAnswers.length === 0 ||
        (!question.multiSelect && selectedAnswers.length !== 1)
      ) {
        setValidationError(`Answer "${question.header}" before submitting.`);
        return;
      }
      if (
        optionLabels.size > 0 &&
        selectedAnswers.filter((answer) => !optionLabels.has(answer)).length >
          (question.isOther ? 1 : 0)
      ) {
        setValidationError(
          `"${question.header}" must use one of the provided options.`,
        );
        return;
      }

      answers[question.id] = selectedAnswers;
    }

    setValidationError(null);
    dispatchSubmission(answers);
  }

  return (
    <article
      className={`message-card user-input-card${pending ? "" : " decided"}`}
    >
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Input request</div>
      <h3>
        {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
      </h3>
      <p className="support-copy">
        {renderHighlightedText(
          message.detail,
          searchQuery,
          searchHighlightTone,
        )}
      </p>

      <div className="user-input-questions">
        {message.questions.map((question) => {
          const field = draft[question.id] ?? {
            customAnswer: "",
            otherSelected: false,
            selectedOptions: [],
          };
          const options = question.options ?? [];
          const inputType = question.isSecret ? "password" : "text";
          const usesOther = !!question.isOther;
          const showFreeform = options.length === 0 || field.otherSelected;

          return (
            <section key={question.id} className="user-input-question">
              <div className="user-input-question-header">
                {renderHighlightedText(
                  question.header,
                  searchQuery,
                  searchHighlightTone,
                )}
              </div>
              <p className="support-copy">
                {renderHighlightedText(
                  question.question,
                  searchQuery,
                  searchHighlightTone,
                )}
              </p>

              {options.length > 0 ? (
                <div className="user-input-options">
                  {options.map((option) => (
                    <label key={option.label} className="user-input-option">
                      <input
                        type={question.multiSelect ? "checkbox" : "radio"}
                        name={`user-input-${instanceId}-${question.id}`}
                        checked={field.selectedOptions.includes(option.label)}
                        disabled={!interactive}
                        onChange={() =>
                          updateField(question.id, {
                            customAnswer: question.multiSelect
                              ? field.customAnswer
                              : "",
                            otherSelected: question.multiSelect
                              ? field.otherSelected
                              : false,
                            selectedOptions: question.multiSelect
                              ? field.selectedOptions.includes(option.label)
                                ? field.selectedOptions.filter(
                                    (selected) => selected !== option.label,
                                  )
                                : [...field.selectedOptions, option.label]
                              : [option.label],
                          })
                        }
                      />
                      <span>
                        <strong>
                          {renderHighlightedText(
                            option.label,
                            searchQuery,
                            searchHighlightTone,
                          )}
                        </strong>
                        <span className="user-input-option-description">
                          {renderHighlightedText(
                            option.description,
                            searchQuery,
                            searchHighlightTone,
                          )}
                        </span>
                      </span>
                    </label>
                  ))}
                  {usesOther ? (
                    <label className="user-input-option">
                      <input
                        type={question.multiSelect ? "checkbox" : "radio"}
                        name={`user-input-${instanceId}-${question.id}`}
                        checked={field.otherSelected}
                        disabled={!interactive}
                        onChange={() =>
                          updateField(question.id, {
                            otherSelected: question.multiSelect
                              ? !field.otherSelected
                              : true,
                            selectedOptions: question.multiSelect
                              ? field.selectedOptions
                              : [],
                          })
                        }
                      />
                      <span>Other</span>
                    </label>
                  ) : null}
                </div>
              ) : null}

              {showFreeform ? (
                <input
                  className="user-input-text"
                  type={inputType}
                  value={field.customAnswer}
                  disabled={!interactive}
                  onChange={(event) =>
                    updateField(question.id, {
                      customAnswer: event.target.value,
                    })
                  }
                />
              ) : null}
            </section>
          );
        })}
      </div>

      {!pending ? (
        <div className="user-input-summary">
          {buildUserInputSummary(message, searchQuery, searchHighlightTone)}
        </div>
      ) : null}

      {validationError ? (
        <p className="approval-result">{validationError}</p>
      ) : null}

      {pending ? (
        <div className="approval-actions">
          <button
            className="approval-button"
            type="button"
            onClick={handleSubmit}
            disabled={dispatched || submissionPending || !actionsEnabled}
          >
            Submit answers
          </button>
          {message.declinable ? (
            <button
              className="approval-button approval-button-reject"
              type="button"
              onClick={() => {
                setValidationError(null);
                dispatchSubmission(null);
              }}
              disabled={dispatched || submissionPending || !actionsEnabled}
            >
              Skip
            </button>
          ) : null}
        </div>
      ) : (
        <p className="approval-result">
          Status:{" "}
          {message.state === "declined"
            ? message.declinable
              ? "Skipped by you"
              : "Resolved by TermAl without human input"
            : message.state}
        </p>
      )}
      {pending && !actionsEnabled ? (
        <p className="support-copy">
          Answer these questions from the live conversation.
        </p>
      ) : null}
    </article>
  );
}

function isJsonObject(
  value: JsonValue | null | undefined,
): value is { [key: string]: JsonValue | undefined } {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function mcpSingleSelectOptions(schema: McpElicitationPrimitiveSchema) {
  if (schema.type !== "string") {
    return [];
  }
  if (schema.oneOf?.length) {
    return schema.oneOf.map((option) => ({
      label: option.title,
      value: option.const,
    }));
  }
  return (schema.enum ?? []).map((value, index) => ({
    label: schema.enumNames?.[index] ?? value,
    value,
  }));
}

function mcpMultiSelectOptions(schema: McpElicitationPrimitiveSchema) {
  if (schema.type !== "array") {
    return [];
  }
  if (schema.items.anyOf?.length) {
    return schema.items.anyOf.map((option) => ({
      label: option.title,
      value: option.const,
    }));
  }
  return (schema.items.enum ?? []).map((value) => ({ label: value, value }));
}

function buildMcpElicitationDraft(
  message: McpElicitationRequestMessage,
): Record<string, McpElicitationDraftField> {
  if (message.request.mode !== "form") {
    return {};
  }

  const submitted = isJsonObject(message.submittedContent)
    ? message.submittedContent
    : {};
  const next: Record<string, McpElicitationDraftField> = {};
  for (const [fieldName, schema] of Object.entries(
    message.request.requestedSchema.properties,
  )) {
    if (!schema) {
      continue;
    }
    const submittedValue = submitted[fieldName];
    switch (schema.type) {
      case "boolean":
        next[fieldName] = {
          selectedOption:
            typeof submittedValue === "boolean"
              ? submittedValue
                ? "true"
                : "false"
              : "",
          selections: [],
          text: "",
        };
        break;
      case "array":
        next[fieldName] = {
          selectedOption: "",
          selections: Array.isArray(submittedValue)
            ? submittedValue.filter(
                (value): value is string => typeof value === "string",
              )
            : (schema.default ?? []),
          text: "",
        };
        break;
      case "number":
      case "integer":
        next[fieldName] = {
          selectedOption: "",
          selections: [],
          text:
            typeof submittedValue === "number"
              ? String(submittedValue)
              : schema.default !== undefined && schema.default !== null
                ? String(schema.default)
                : "",
        };
        break;
      case "string": {
        const options = mcpSingleSelectOptions(schema);
        const submittedText =
          typeof submittedValue === "string" ? submittedValue : "";
        next[fieldName] = {
          selectedOption: options.some(
            (option) => option.value === submittedText,
          )
            ? submittedText
            : "",
          selections: [],
          text:
            submittedText ||
            (typeof schema.default === "string" ? schema.default : ""),
        };
        break;
      }
    }
  }
  return next;
}

function formatMcpElicitationSummaryValue(value: JsonValue) {
  if (Array.isArray(value)) {
    return value.join(", ");
  }
  if (typeof value === "boolean") {
    return value ? "Yes" : "No";
  }
  if (value === null) {
    return "null";
  }
  if (typeof value === "object") {
    return JSON.stringify(value);
  }
  return String(value);
}

function buildMcpElicitationSummary(
  message: McpElicitationRequestMessage,
  searchQuery: string,
  searchHighlightTone: SearchHighlightTone,
) {
  if (
    !isJsonObject(message.submittedContent) ||
    message.request.mode !== "form"
  ) {
    return null;
  }
  const submittedContent = message.submittedContent;

  return Object.entries(message.request.requestedSchema.properties)
    .filter(([fieldName]) => submittedContent[fieldName] !== undefined)
    .map(([fieldName, schema]) => {
      const value = submittedContent[fieldName];
      if (!schema || value === undefined) {
        return null;
      }
      return (
        <div key={fieldName} className="user-input-summary-row">
          <div className="user-input-summary-header">
            {renderHighlightedText(
              schema.title ?? fieldName,
              searchQuery,
              searchHighlightTone,
            )}
          </div>
          <div className="user-input-summary-value">
            {renderHighlightedText(
              formatMcpElicitationSummaryValue(value),
              searchQuery,
              searchHighlightTone,
            )}
          </div>
        </div>
      );
    });
}

export function McpElicitationRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: McpElicitationRequestMessage;
  onSubmit: (
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [draft, setDraft] = useState<Record<string, McpElicitationDraftField>>(
    () => buildMcpElicitationDraft(message),
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const pending = message.state === "pending";

  useEffect(() => {
    setDraft(buildMcpElicitationDraft(message));
    setValidationError(null);
  }, [message]);

  function updateField(
    fieldName: string,
    nextField: Partial<McpElicitationDraftField>,
  ) {
    setDraft((current) => ({
      ...current,
      [fieldName]: {
        selectedOption: current[fieldName]?.selectedOption ?? "",
        selections: current[fieldName]?.selections ?? [],
        text: current[fieldName]?.text ?? "",
        ...nextField,
      },
    }));
  }

  function handleSubmit(action: McpElicitationAction) {
    if (message.request.mode !== "form" || action !== "accept") {
      setValidationError(null);
      onSubmit(message.id, action);
      return;
    }

    const required = new Set(message.request.requestedSchema.required ?? []);
    const content: Record<string, JsonValue> = {};

    for (const [fieldName, schema] of Object.entries(
      message.request.requestedSchema.properties,
    )) {
      if (!schema) {
        continue;
      }
      const field = draft[fieldName] ?? {
        selectedOption: "",
        selections: [],
        text: "",
      };
      switch (schema.type) {
        case "string": {
          const options = mcpSingleSelectOptions(schema);
          const hasOptions = options.length > 0;
          const rawValue = hasOptions
            ? field.selectedOption
            : field.text.trim();
          if (!rawValue) {
            if (required.has(fieldName)) {
              setValidationError(
                `Answer "${schema.title ?? fieldName}" before accepting.`,
              );
              return;
            }
            break;
          }
          if (
            hasOptions &&
            !options.some((option) => option.value === rawValue)
          ) {
            setValidationError(
              `"${schema.title ?? fieldName}" must use one of the provided options.`,
            );
            return;
          }
          const valueLength = Array.from(rawValue).length;
          if (schema.minLength != null && valueLength < schema.minLength) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at least ${schema.minLength} characters.`,
            );
            return;
          }
          if (schema.maxLength != null && valueLength > schema.maxLength) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at most ${schema.maxLength} characters.`,
            );
            return;
          }
          content[fieldName] = rawValue;
          break;
        }
        case "number":
        case "integer": {
          const rawValue = field.text.trim();
          if (!rawValue) {
            if (required.has(fieldName)) {
              setValidationError(
                `Answer "${schema.title ?? fieldName}" before accepting.`,
              );
              return;
            }
            break;
          }
          const numericValue = Number(rawValue);
          if (!Number.isFinite(numericValue)) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be a valid number.`,
            );
            return;
          }
          if (schema.type === "integer" && !Number.isInteger(numericValue)) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be a whole number.`,
            );
            return;
          }
          if (schema.minimum != null && numericValue < schema.minimum) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at least ${schema.minimum}.`,
            );
            return;
          }
          if (schema.maximum != null && numericValue > schema.maximum) {
            setValidationError(
              `"${schema.title ?? fieldName}" must be at most ${schema.maximum}.`,
            );
            return;
          }
          content[fieldName] = numericValue;
          break;
        }
        case "boolean": {
          if (!field.selectedOption) {
            if (required.has(fieldName)) {
              setValidationError(
                `Answer "${schema.title ?? fieldName}" before accepting.`,
              );
              return;
            }
            break;
          }
          content[fieldName] = field.selectedOption === "true";
          break;
        }
        case "array": {
          const options = mcpMultiSelectOptions(schema);
          if (field.selections.length === 0) {
            if (required.has(fieldName) || (schema.minItems ?? 0) > 0) {
              setValidationError(
                `Choose at least one option for "${schema.title ?? fieldName}".`,
              );
              return;
            }
            break;
          }
          if (
            !field.selections.every((selection) =>
              options.some((option) => option.value === selection),
            )
          ) {
            setValidationError(
              `"${schema.title ?? fieldName}" must use one of the provided options.`,
            );
            return;
          }
          if (
            schema.minItems != null &&
            field.selections.length < schema.minItems
          ) {
            setValidationError(
              `"${schema.title ?? fieldName}" must include at least ${schema.minItems} selections.`,
            );
            return;
          }
          if (
            schema.maxItems != null &&
            field.selections.length > schema.maxItems
          ) {
            setValidationError(
              `"${schema.title ?? fieldName}" must include at most ${schema.maxItems} selections.`,
            );
            return;
          }
          content[fieldName] = field.selections;
          break;
        }
      }
    }

    setValidationError(null);
    onSubmit(message.id, action, content);
  }

  return (
    <article
      className={`message-card user-input-card${pending ? "" : " decided"}`}
    >
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">MCP input</div>
      <h3>
        {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
      </h3>
      <p className="support-copy">
        {renderHighlightedText(
          message.detail,
          searchQuery,
          searchHighlightTone,
        )}
      </p>

      {message.request.mode === "url" ? (
        <p className="support-copy">
          <a href={message.request.url} target="_blank" rel="noreferrer">
            {renderHighlightedText(
              message.request.url,
              searchQuery,
              searchHighlightTone,
            )}
          </a>
        </p>
      ) : (
        <div className="user-input-questions">
          {Object.entries(message.request.requestedSchema.properties).map(
            ([fieldName, schema]) => {
              if (!schema) {
                return null;
              }
              const field = draft[fieldName] ?? {
                selectedOption: "",
                selections: [],
                text: "",
              };
              const label = schema.title ?? fieldName;
              const description = schema.description ?? message.request.message;
              const singleOptions = mcpSingleSelectOptions(schema);
              const multiOptions = mcpMultiSelectOptions(schema);
              return (
                <section key={fieldName} className="user-input-question">
                  <div className="user-input-question-header">
                    {renderHighlightedText(
                      label,
                      searchQuery,
                      searchHighlightTone,
                    )}
                  </div>
                  {description ? (
                    <p className="support-copy">
                      {renderHighlightedText(
                        description,
                        searchQuery,
                        searchHighlightTone,
                      )}
                    </p>
                  ) : null}

                  {schema.type === "string" && singleOptions.length > 0 ? (
                    <div className="user-input-options">
                      {singleOptions.map((option) => (
                        <label key={option.value} className="user-input-option">
                          <input
                            type="radio"
                            name={`mcp-elicitation-${message.id}-${fieldName}`}
                            checked={field.selectedOption === option.value}
                            disabled={!pending}
                            onChange={() =>
                              updateField(fieldName, {
                                selectedOption: option.value,
                              })
                            }
                          />
                          <span>
                            {renderHighlightedText(
                              option.label,
                              searchQuery,
                              searchHighlightTone,
                            )}
                          </span>
                        </label>
                      ))}
                    </div>
                  ) : null}

                  {schema.type === "array" ? (
                    <div className="user-input-options">
                      {multiOptions.map((option) => (
                        <label key={option.value} className="user-input-option">
                          <input
                            type="checkbox"
                            checked={field.selections.includes(option.value)}
                            disabled={!pending}
                            onChange={(event) =>
                              updateField(fieldName, {
                                selections: event.target.checked
                                  ? [...field.selections, option.value]
                                  : field.selections.filter(
                                      (value) => value !== option.value,
                                    ),
                              })
                            }
                          />
                          <span>
                            {renderHighlightedText(
                              option.label,
                              searchQuery,
                              searchHighlightTone,
                            )}
                          </span>
                        </label>
                      ))}
                    </div>
                  ) : null}

                  {schema.type === "boolean" ? (
                    <div className="user-input-options">
                      <label className="user-input-option">
                        <input
                          type="radio"
                          name={`mcp-elicitation-${message.id}-${fieldName}`}
                          checked={field.selectedOption === "true"}
                          disabled={!pending}
                          onChange={() =>
                            updateField(fieldName, { selectedOption: "true" })
                          }
                        />
                        <span>Yes</span>
                      </label>
                      <label className="user-input-option">
                        <input
                          type="radio"
                          name={`mcp-elicitation-${message.id}-${fieldName}`}
                          checked={field.selectedOption === "false"}
                          disabled={!pending}
                          onChange={() =>
                            updateField(fieldName, { selectedOption: "false" })
                          }
                        />
                        <span>No</span>
                      </label>
                    </div>
                  ) : null}

                  {schema.type === "number" ||
                  schema.type === "integer" ||
                  (schema.type === "string" && singleOptions.length === 0) ? (
                    <input
                      className="user-input-text"
                      type={
                        schema.type === "number" || schema.type === "integer"
                          ? "number"
                          : "text"
                      }
                      value={field.text}
                      min={
                        schema.type === "number" || schema.type === "integer"
                          ? (schema.minimum ?? undefined)
                          : undefined
                      }
                      max={
                        schema.type === "number" || schema.type === "integer"
                          ? (schema.maximum ?? undefined)
                          : undefined
                      }
                      minLength={
                        schema.type === "string"
                          ? (schema.minLength ?? undefined)
                          : undefined
                      }
                      maxLength={
                        schema.type === "string"
                          ? (schema.maxLength ?? undefined)
                          : undefined
                      }
                      disabled={!pending}
                      onChange={(event) =>
                        updateField(fieldName, { text: event.target.value })
                      }
                    />
                  ) : null}
                </section>
              );
            },
          )}
        </div>
      )}

      {!pending ? (
        <div className="user-input-summary">
          <div className="user-input-summary-row">
            <div className="user-input-summary-header">Decision</div>
            <div className="user-input-summary-value">
              {message.submittedAction ?? message.state}
            </div>
          </div>
          {buildMcpElicitationSummary(
            message,
            searchQuery,
            searchHighlightTone,
          )}
        </div>
      ) : null}

      {validationError ? (
        <p className="approval-result">{validationError}</p>
      ) : null}

      {pending ? (
        <div className="approval-actions">
          <button
            className="approval-button"
            type="button"
            onClick={() => handleSubmit("accept")}
          >
            Accept
          </button>
          <button
            className="approval-button"
            type="button"
            onClick={() => handleSubmit("decline")}
          >
            Decline
          </button>
          <button
            className="approval-button approval-button-reject"
            type="button"
            onClick={() => handleSubmit("cancel")}
          >
            Cancel
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

function formatJsonValueForEditor(value: JsonValue | null | undefined) {
  return JSON.stringify(value ?? {}, null, 2);
}

export function CodexAppRequestCard({
  message,
  onSubmit,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CodexAppRequestMessage;
  onSubmit: (messageId: string, result: JsonValue) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const pending = message.state === "pending";
  const [draft, setDraft] = useState(() =>
    formatJsonValueForEditor(message.submittedResult),
  );
  const [validationError, setValidationError] = useState<string | null>(null);

  useEffect(() => {
    setDraft(formatJsonValueForEditor(message.submittedResult));
    setValidationError(null);
  }, [message]);

  function handleSubmit() {
    try {
      const parsed = JSON.parse(draft) as JsonValue;
      setValidationError(null);
      onSubmit(message.id, parsed);
    } catch {
      setValidationError("Response must be valid JSON.");
    }
  }

  return (
    <article
      className={`message-card user-input-card${pending ? "" : " decided"}`}
    >
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Codex request</div>
      <h3>
        {renderHighlightedText(message.title, searchQuery, searchHighlightTone)}
      </h3>
      <p className="support-copy">
        {renderHighlightedText(
          message.detail,
          searchQuery,
          searchHighlightTone,
        )}
      </p>

      <div className="user-input-summary">
        <div className="user-input-summary-row">
          <div className="user-input-summary-header">Method</div>
          <div className="user-input-summary-value">{message.method}</div>
        </div>
      </div>

      <div className="codex-request-json-block">
        <div className="user-input-summary-header">Request payload</div>
        <pre>{formatJsonValueForEditor(message.params)}</pre>
      </div>

      {pending ? (
        <label className="codex-request-editor">
          <span className="user-input-summary-header">JSON result</span>
          <textarea
            className="codex-request-textarea"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            spellCheck={false}
          />
        </label>
      ) : (
        <div className="codex-request-json-block">
          <div className="user-input-summary-header">Submitted result</div>
          <pre>{formatJsonValueForEditor(message.submittedResult)}</pre>
        </div>
      )}

      {validationError ? (
        <p className="approval-result">{validationError}</p>
      ) : null}

      {pending ? (
        <div className="approval-actions">
          <button
            className="approval-button"
            type="button"
            onClick={handleSubmit}
          >
            Submit JSON result
          </button>
        </div>
      ) : (
        <p className="approval-result">Status: {message.state}</p>
      )}
    </article>
  );
}

// Split from ../preferences-panels.tsx to keep agent default model settings
// separate from the broader preferences panel collection.
import {
  useEffect,
  useMemo,
  useState,
} from "react";

import {
  DEFAULT_MODEL_PREFERENCE,
  isDefaultModelPreference,
  MAX_DEFAULT_MODEL_PREFERENCE_CHARS,
  sessionModelComboboxOptions,
  type ComboboxOption,
} from "../session-model-utils";
import type {
  AgentType,
  Session,
} from "../types";
import { ThemedCombobox } from "./themed-combobox";

function normalizeDefaultModelPreferenceDraft(value: string) {
  return isDefaultModelPreference(value)
    ? DEFAULT_MODEL_PREFERENCE
    : value.trim();
}

function displayDefaultModelPreference(value: string) {
  return isDefaultModelPreference(value)
    ? DEFAULT_MODEL_PREFERENCE
    : value;
}

function defaultModelComboboxOptions(
  agent: AgentType,
  value: string,
  modelOptions: readonly ComboboxOption[] = [],
): ComboboxOption[] {
  const normalizedValue = normalizeDefaultModelPreferenceDraft(value);
  const options = new Map<string, ComboboxOption>();

  options.set(DEFAULT_MODEL_PREFERENCE, {
    label: "Default",
    value: DEFAULT_MODEL_PREFERENCE,
    description: `Let ${agent} choose its built-in default`,
  });

  for (const option of modelOptions) {
    const normalizedOptionValue = normalizeDefaultModelPreferenceDraft(option.value);
    if (options.has(normalizedOptionValue)) {
      continue;
    }

    options.set(normalizedOptionValue, {
      ...option,
      label:
        isDefaultModelPreference(option.value) || option.label.trim().toLowerCase() === "default"
          ? "Default"
          : option.label,
      value: normalizedOptionValue,
    });
  }

  if (!options.has(normalizedValue)) {
    options.set(normalizedValue, {
      label: isDefaultModelPreference(normalizedValue) ? "Default" : normalizedValue,
      value: normalizedValue,
    });
  }

  return Array.from(options.values());
}

export function defaultModelOptionsFromSessions(
  agent: AgentType,
  sessions: readonly Session[],
  value: string,
): {
  hasLiveModelList: boolean;
  options: ComboboxOption[];
} {
  const liveModelOptions = sessions
    .filter((session) => session.agent === agent)
    .flatMap((session) =>
      session.modelOptions?.length
        ? sessionModelComboboxOptions(session.modelOptions, session.model)
        : [],
    );

  return {
    hasLiveModelList: liveModelOptions.length > 0,
    options: defaultModelComboboxOptions(agent, value, liveModelOptions),
  };
}

export function AgentDefaultModelControl({
  agent,
  hasLiveModelList = false,
  id,
  modelOptions,
  value,
  onChange,
}: {
  agent: AgentType;
  hasLiveModelList?: boolean;
  id: string;
  modelOptions?: readonly ComboboxOption[];
  value: string;
  onChange: (nextValue: string) => void;
}) {
  const normalizedValue = normalizeDefaultModelPreferenceDraft(value);
  const [customModel, setCustomModel] = useState(displayDefaultModelPreference(normalizedValue));
  const hintId = `${id}-hint`;
  const customId = `${id}-custom`;
  const customLabelId = `${customId}-label`;
  const selectOptions = useMemo(
    () => defaultModelComboboxOptions(agent, value, modelOptions),
    [agent, modelOptions, value],
  );
  const trimmedCustomModel = customModel.trim();
  const normalizedCustomModel = normalizeDefaultModelPreferenceDraft(trimmedCustomModel);
  const canApplyCustomModel =
    trimmedCustomModel.length > 0 && normalizedCustomModel !== normalizedValue;
  const customModelKnown = selectOptions.some(
    (option) => option.value === normalizedCustomModel,
  );
  const validationMessage =
    trimmedCustomModel.length === 0
      ? null
      : normalizedCustomModel === normalizedValue
        ? `${trimmedCustomModel} is already the configured default model.`
        : !customModelKnown && hasLiveModelList
          ? `${trimmedCustomModel} is not in the current live model list. TermAl will still try it for new ${agent} sessions.`
          : null;
  const validationTone =
    validationMessage && !customModelKnown && hasLiveModelList ? "warning" : "info";

  useEffect(() => {
    setCustomModel(displayDefaultModelPreference(normalizedValue));
  }, [normalizedValue]);

  function applyCustomModel() {
    if (!canApplyCustomModel) {
      return;
    }

    onChange(normalizedCustomModel);
  }

  return (
    <div className="session-control-group">
      <label className="session-control-label" htmlFor={id}>
        Default model
      </label>
      <ThemedCombobox
        id={id}
        className="prompt-settings-select"
        value={displayDefaultModelPreference(normalizedValue)}
        options={selectOptions}
        aria-label={`${agent} default model`}
        onChange={(nextValue) => {
          onChange(normalizeDefaultModelPreferenceDraft(nextValue));
        }}
      />
      <div className="session-model-custom">
        <label id={customLabelId} className="session-control-label" htmlFor={customId}>
          {agent} custom default model
        </label>
        <div className="session-model-custom-row">
          <input
            id={customId}
            className="themed-input session-model-custom-input"
            type="text"
            value={customModel}
            placeholder={`${agent.toLowerCase()} model id`}
            maxLength={MAX_DEFAULT_MODEL_PREFERENCE_CHARS}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            aria-labelledby={customLabelId}
            onChange={(event) => setCustomModel(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== "Enter") {
                return;
              }

              event.preventDefault();
              applyCustomModel();
            }}
          />
          <button
            type="button"
            className="ghost-button session-model-custom-apply"
            disabled={!canApplyCustomModel}
            aria-label={`Apply ${agent} default model`}
            onClick={applyCustomModel}
          >
            Apply
          </button>
        </div>
        {validationMessage ? (
          <p
            className={`session-model-custom-validation ${validationTone}`}
            aria-live="polite"
          >
            {validationMessage}
          </p>
        ) : null}
      </div>
      <p id={hintId} className="session-control-hint">
        Select a known model, enter an exact model id, or choose <code>Default</code> to let {agent} use its built-in default.
      </p>
    </div>
  );
}

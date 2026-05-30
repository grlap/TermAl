// Split from ../preferences-panels.tsx to keep agent default model settings
// separate from the broader preferences panel collection.
import { useMemo } from "react";

import {
  DEFAULT_MODEL_PREFERENCE,
  isDefaultModelPreference,
  NEW_SESSION_MODEL_OPTIONS,
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
  const knownModelOptions = [
    ...NEW_SESSION_MODEL_OPTIONS[agent],
    ...modelOptions,
  ];

  options.set(DEFAULT_MODEL_PREFERENCE, {
    label: "Default",
    value: DEFAULT_MODEL_PREFERENCE,
    description: `Let ${agent} choose its built-in default`,
  });

  for (const option of knownModelOptions) {
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
    options: defaultModelComboboxOptions(agent, value, liveModelOptions),
  };
}

export function AgentDefaultModelControl({
  agent,
  id,
  modelOptions,
  value,
  onChange,
}: {
  agent: AgentType;
  id: string;
  modelOptions?: readonly ComboboxOption[];
  value: string;
  onChange: (nextValue: string) => void;
}) {
  const normalizedValue = normalizeDefaultModelPreferenceDraft(value);
  const hintId = `${id}-hint`;
  const selectOptions = useMemo(
    () => defaultModelComboboxOptions(agent, value, modelOptions),
    [agent, modelOptions, value],
  );

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
      <p id={hintId} className="session-control-hint">
        Select a known model or choose <code>Default</code> to let {agent} use its built-in default.
      </p>
    </div>
  );
}

// State-shape helpers + localStorage reader for the
// `<OrchestratorTemplatesPanel>`. Nothing here calls into React —
// consumers feed these functions the templates they loaded and the
// state they want to restore, and receive back a validated
// PanelState / InitialPanelState. The panel owns the React state
// wiring, the `useState` initializers, and the throttled writer
// side of the persistence loop.
//
// What this file owns:
//   - `STATE_KEY_PREFIX` — the `"termal-orchestrator-panel-state:"`
//     prefix applied to the caller-provided `persistenceKey` when
//     composing the localStorage key. Both the reader here and the
//     writer back in the panel compose keys through this constant,
//     so they can't drift.
//   - `PanelState` — the persisted `{ draft, selectedNodeId,
//     selectedTemplateId }` record written to localStorage.
//   - `InitialPanelState` — `PanelState` plus the `savedDraft`
//     baseline the panel diffs against to decide whether the
//     "Save" button should be enabled.
//   - `emptyDraft` — a fresh, empty `OrchestratorTemplateDraft`
//     (empty name / description / projectId and empty session +
//     transition lists).
//   - `orchestratorSessionModelOptions` — builds the combobox
//     option list for a given agent + currently-selected model:
//     starts with a synthetic "Default" option, fills in either
//     live per-session options or the static
//     `NEW_SESSION_MODEL_OPTIONS` fallback, and appends the
//     currently-selected model (formatted via
//     `formatSessionModelOptionLabel`) if it isn't already listed.
//     De-dupes on `value.trim().toLowerCase()` with a special
//     `__default__` bucket that collapses empty / literal
//     "default" entries together.
//   - `buildOrchestratorModelOptionsByAgent` — distills a
//     `readonly Session[]` into a `Map<AgentType, ComboboxOption[]>`
//     keyed by agent, de-duping by option value. Used by the
//     panel to seed the per-agent combobox option lists shown in
//     the session editor.
//   - `EMPTY_ORCHESTRATOR_MODEL_OPTIONS` — internal empty-map
//     sentinel used as the default argument to
//     `orchestratorSessionModelOptions`.
//   - `templateToDraft` — clones an `OrchestratorTemplate` into a
//     mutable `OrchestratorTemplateDraft`, filling in empty
//     strings for nullable model + promptTemplate fields and
//     shallow-cloning each session's `position`.
//   - `resolveInitialState` — the big "what should the panel show
//     on first mount?" decision tree. Prefers the restored
//     localStorage payload when it is valid; otherwise falls back
//     to an empty draft (for `startMode === "new"`) or to the
//     first / initialTemplateId-matching template in the loaded
//     list.
//   - `savedDraftForTemplateId` — helper that returns either the
//     drafted-from-template snapshot or `emptyDraft()` when the
//     id is missing or unknown.
//   - `finalizePanelState` — coerces a draft + selection pair
//     into a `PanelState`, snapping `selectedNodeId` to the first
//     session in the draft when the requested id is no longer
//     present.
//   - `readState` — the localStorage reader. Parses JSON, walks
//     every field with the persistence-schema guards, remaps
//     position via `clampPosition` to re-apply the board frame,
//     and returns the finalised `PanelState` on success or `null`
//     on any schema miss or parse error.
//
// What this file does NOT own:
//   - `PendingPanelPersistence` — the `{ stateKey, serialized }`
//     pending-write shape — stays with the panel because it is
//     consumed entirely from ref-based React writer state.
//   - `persistPanelStateImmediately`, `flushPersistedPanelStateRef`,
//     and the throttled localStorage writer logic — stays with the
//     panel since it is closure-over-refs stateful.
//   - `NEW_SESSION_MODEL_OPTIONS`, `formatSessionModelOptionLabel`,
//     `ComboboxOption` — live in `../session-model-utils`.
//   - `clampPosition`, `isPersistedSessionTemplate`,
//     `isTransitionTemplate` — live in the sibling
//     `./orchestrator-template-edits.ts` and
//     `./orchestrator-template-persistence-schema.ts` modules and
//     are imported here.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`.
// Same state-key prefix, same JSON shape, same default
// "Default" synthetic option, same `__default__` dedup bucket,
// same fallback ordering in `resolveInitialState`.

import {
  formatSessionModelOptionLabel,
  NEW_SESSION_MODEL_OPTIONS,
  type ComboboxOption,
} from "../session-model-utils";
import type {
  AgentType,
  OrchestratorTemplate,
  OrchestratorTemplateDraft,
  Session,
} from "../types";
import { clampPosition } from "./orchestrator-template-edits";
import {
  isPersistedSessionTemplate,
  isTransitionTemplate,
} from "./orchestrator-template-persistence-schema";

export const STATE_KEY_PREFIX = "termal-orchestrator-panel-state:";

export type PanelState = {
  draft: OrchestratorTemplateDraft;
  selectedNodeId: string | null;
  selectedTemplateId: string | null;
};

export type InitialPanelState = PanelState & {
  savedDraft: OrchestratorTemplateDraft;
};

export function emptyDraft(): OrchestratorTemplateDraft {
  return {
    name: "",
    description: "",
    projectId: null,
    sessions: [],
    transitions: [],
  };
}

const EMPTY_ORCHESTRATOR_MODEL_OPTIONS = new Map<
  AgentType,
  readonly ComboboxOption[]
>();

export function orchestratorSessionModelOptions(
  agent: AgentType,
  model?: string | null,
  modelOptionsByAgent: ReadonlyMap<AgentType, readonly ComboboxOption[]> = EMPTY_ORCHESTRATOR_MODEL_OPTIONS,
): ComboboxOption[] {
  const normalizedModel = model?.trim() ?? "";
  const liveOptions = modelOptionsByAgent.get(agent) ?? [];
  const sourceOptions = liveOptions.length
    ? liveOptions
    : NEW_SESSION_MODEL_OPTIONS[agent];
  const options: ComboboxOption[] = [];
  const seenValues = new Set<string>();
  const pushOption = (option: ComboboxOption) => {
    const normalizedValue = option.value.trim().toLowerCase();
    const isDefaultLike =
      normalizedValue === "" ||
      (normalizedValue === "default" &&
        option.label.trim().toLowerCase() === "default");
    const key = isDefaultLike ? "__default__" : normalizedValue;
    if (seenValues.has(key)) {
      return;
    }

    seenValues.add(key);
    options.push(option);
  };

  pushOption(
    {
      label: "Default",
      value: "",
      description: "Use this assistant's default model",
    },
  );
  for (const option of sourceOptions) {
    pushOption(option);
  }

  if (
    normalizedModel &&
    !options.some(
      (option) =>
        option.value.trim().toLowerCase() === normalizedModel.toLowerCase(),
    )
  ) {
    pushOption({
      label: formatSessionModelOptionLabel(normalizedModel),
      value: normalizedModel,
    });
  }

  return options;
}

export function buildOrchestratorModelOptionsByAgent(
  sessions: readonly Session[],
): ReadonlyMap<AgentType, readonly ComboboxOption[]> {
  const optionsByAgent = new Map<AgentType, ComboboxOption[]>();

  for (const session of sessions) {
    if (!session.modelOptions?.length) {
      continue;
    }

    const agentOptions = optionsByAgent.get(session.agent) ?? [];
    const seenValues = new Set(agentOptions.map((option) => option.value));

    for (const option of session.modelOptions) {
      if (seenValues.has(option.value)) {
        continue;
      }

      seenValues.add(option.value);
      agentOptions.push({
        label: option.label,
        value: option.value,
        description: option.description ?? undefined,
        badges: option.badges?.length ? option.badges : undefined,
      });
    }

    optionsByAgent.set(session.agent, agentOptions);
  }

  return optionsByAgent;
}

export function templateToDraft(
  template: OrchestratorTemplate,
): OrchestratorTemplateDraft {
  return {
    name: template.name,
    description: template.description,
    projectId: template.projectId ?? null,
    sessions: template.sessions.map((session) => ({
      ...session,
      model: session.model ?? "",
      position: { ...session.position },
    })),
    transitions: template.transitions.map((transition) => ({
      ...transition,
      promptTemplate: transition.promptTemplate ?? "",
    })),
  };
}

export function resolveInitialState(
  templates: OrchestratorTemplate[],
  initialTemplateId: string | null,
  restored: PanelState | null,
  startMode: "browse" | "edit" | "new",
): InitialPanelState {
  if (restored) {
    const restoredTemplateId =
      typeof restored.selectedTemplateId === "string"
        ? restored.selectedTemplateId
        : null;
    const selectedTemplateId =
      restoredTemplateId &&
      templates.some((template) => template.id === restoredTemplateId)
        ? restoredTemplateId
        : null;
    if (restoredTemplateId && !selectedTemplateId) {
      const savedDraft = emptyDraft();
      return {
        ...finalizePanelState(restored.draft, null, restored.selectedNodeId),
        savedDraft,
      };
    }
    const panelState = finalizePanelState(
      restored.draft,
      selectedTemplateId,
      restored.selectedNodeId,
    );
    return {
      ...panelState,
      savedDraft: savedDraftForTemplateId(templates, selectedTemplateId),
    };
  }

  if (startMode === "new") {
    const draft = emptyDraft();
    return { ...finalizePanelState(draft, null, null), savedDraft: draft };
  }

  const selectedTemplate =
    (initialTemplateId
      ? templates.find((template) => template.id === initialTemplateId)
      : null) ??
    templates[0] ??
    null;

  if (!selectedTemplate) {
    const draft = emptyDraft();
    return { ...finalizePanelState(draft, null, null), savedDraft: draft };
  }

  const savedDraft = templateToDraft(selectedTemplate);
  return {
    ...finalizePanelState(
      savedDraft,
      selectedTemplate.id,
      selectedTemplate.sessions[0]?.id ?? null,
    ),
    savedDraft,
  };
}

export function savedDraftForTemplateId(
  templates: OrchestratorTemplate[],
  selectedTemplateId: string | null,
): OrchestratorTemplateDraft {
  if (!selectedTemplateId) {
    return emptyDraft();
  }

  const selectedTemplate = templates.find(
    (template) => template.id === selectedTemplateId,
  );
  return selectedTemplate ? templateToDraft(selectedTemplate) : emptyDraft();
}

export function finalizePanelState(
  draft: OrchestratorTemplateDraft,
  selectedTemplateId: string | null,
  selectedNodeId: string | null,
): PanelState {
  const nextSelectedNodeId =
    selectedNodeId &&
    draft.sessions.some((session) => session.id === selectedNodeId)
      ? selectedNodeId
      : (draft.sessions[0]?.id ?? null);

  return {
    draft,
    selectedNodeId: nextSelectedNodeId,
    selectedTemplateId,
  };
}

export function readState(stateKey: string): PanelState | null {
  try {
    const raw = window.localStorage.getItem(stateKey);
    if (!raw) {
      return null;
    }
    const parsed = JSON.parse(raw) as Partial<PanelState>;
    if (!parsed.draft || typeof parsed.draft !== "object") {
      return null;
    }

    const draft = parsed.draft as Partial<OrchestratorTemplateDraft>;
    if (
      typeof draft.name !== "string" ||
      typeof draft.description !== "string" ||
      !Array.isArray(draft.sessions) ||
      !Array.isArray(draft.transitions)
    ) {
      return null;
    }

    if (
      !draft.sessions.every(isPersistedSessionTemplate) ||
      !draft.transitions.every(isTransitionTemplate)
    ) {
      return null;
    }

    return finalizePanelState(
      {
        name: draft.name,
        description: draft.description,
        projectId:
          typeof draft.projectId === "string" && draft.projectId.trim()
            ? draft.projectId
            : null,
        sessions: draft.sessions.map((session) => ({
          id: session.id,
          name: session.name,
          agent: session.agent,
          model: session.model ?? "",
          instructions: session.instructions,
          autoApprove: session.autoApprove,
          inputMode: session.inputMode,
          position: clampPosition(session.position.x, session.position.y),
        })),
        transitions: draft.transitions.map((transition) => ({
          id: transition.id,
          fromSessionId: transition.fromSessionId,
          toSessionId: transition.toSessionId,
          trigger: transition.trigger,
          resultMode: transition.resultMode,
          promptTemplate: transition.promptTemplate ?? "",
          ...(transition.fromAnchor != null
            ? { fromAnchor: transition.fromAnchor }
            : {}),
          ...(transition.toAnchor != null
            ? { toAnchor: transition.toAnchor }
            : {}),
        })),
      },
      typeof parsed.selectedTemplateId === "string"
        ? parsed.selectedTemplateId
        : null,
      typeof parsed.selectedNodeId === "string" ? parsed.selectedNodeId : null,
    );
  } catch {
    return null;
  }
}

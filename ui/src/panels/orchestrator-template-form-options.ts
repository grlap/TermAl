// Static combobox option lists consumed by the orchestrator
// template editor's form surface: agent picker, per-session input
// mode, per-transition result mode.
//
// What this file owns:
//   - `AGENT_OPTIONS` — the read-only `{ label, value }` list for
//     the agent combobox, typed as
//     `ReadonlyArray<{ label: string; value: AgentType }>` and
//     covering the four P0 agents (Claude / Codex / Cursor /
//     Gemini).
//   - `AGENT_OPTIONS_EXHAUSTIVE` — an
//     `ExhaustiveValueCoverage<AgentType, typeof AGENT_OPTIONS>`
//     witness that fails the build if a new `AgentType` lands
//     without a matching `AGENT_OPTIONS` entry.
//   - `INPUT_MODE_OPTIONS` — `{ label, value }` pairs for the
//     session input-mode combobox ("Queue" / "Consolidate").
//   - `RESULT_MODE_OPTIONS` — `{ label, value }` pairs for the
//     transition result-mode combobox (Last response / Summary /
//     Summary + last response / No result).
//
// What this file does NOT own:
//   - The `<Combobox>` rendering itself — that stays in the
//     panel's JSX.
//   - Any form state or template-draft mutation — those stay with
//     `./orchestrator-template-edits` and
//     `<OrchestratorTemplatesPanel>`.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`.
// Same labels, same values, same agent ordering.

import type {
  AgentType,
  ExhaustiveValueCoverage,
  OrchestratorSessionInputMode,
  OrchestratorTransitionResultMode,
} from "../types";

export const AGENT_OPTIONS = [
  { label: "Claude", value: "Claude" },
  { label: "Codex", value: "Codex" },
  { label: "Cursor", value: "Cursor" },
  { label: "Gemini", value: "Gemini" },
] as const satisfies ReadonlyArray<{ label: string; value: AgentType }>;

export const AGENT_OPTIONS_EXHAUSTIVE: ExhaustiveValueCoverage<
  AgentType,
  typeof AGENT_OPTIONS
> = true;

export const INPUT_MODE_OPTIONS: ReadonlyArray<{
  label: string;
  value: OrchestratorSessionInputMode;
}> = [
  { label: "Queue", value: "queue" },
  { label: "Consolidate", value: "consolidate" },
];

export const RESULT_MODE_OPTIONS: ReadonlyArray<{
  label: string;
  value: OrchestratorTransitionResultMode;
}> = [
  { label: "Last response", value: "lastResponse" },
  { label: "Summary", value: "summary" },
  { label: "Summary + last response", value: "summaryAndLastResponse" },
  { label: "No result", value: "none" },
];

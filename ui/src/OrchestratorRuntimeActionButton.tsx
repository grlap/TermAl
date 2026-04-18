// The pause / resume / stop button rendered in the control panel's
// orchestrator group header.
//
// What this file owns:
//   - The aria-label string: `"Pause orchestration <id>"` /
//     `"Resume orchestration <id>"` / `"Stop orchestration <id>"`.
//   - The `title` tooltip copy, which swaps to the
//     `-ing orchestration` present-continuous form while the
//     action is pending.
//   - Forwarding everything else (action variant, pending spinner,
//     disabled state, click handler) to the shared
//     `RuntimeActionButton`, using the
//     `"session-orchestrator-group-action"` class prefix so the
//     orchestrator-group styling applies.
//
// What this file does NOT own:
//   - The underlying button chrome, icons, and pending spinner —
//     those live in `./runtime-action-button`. This component is
//     a thin wrapper that specialises the labels / tooltips for
//     the orchestrator-group context.
//   - The caller's state machine (what makes an action "pending",
//     when it's disabled, what `onClick` does) — App.tsx owns
//     those.
//
// Split out of `ui/src/App.tsx`. Same signature and behaviour as
// the inline definition it replaced.

import { RuntimeActionButton } from "./runtime-action-button";

export function OrchestratorRuntimeActionButton({
  action,
  orchestratorId,
  isPending,
  disabled,
  onClick,
}: {
  action: "pause" | "resume" | "stop";
  orchestratorId: string;
  isPending: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  const label =
    action === "pause"
      ? `Pause orchestration ${orchestratorId}`
      : action === "resume"
        ? `Resume orchestration ${orchestratorId}`
        : `Stop orchestration ${orchestratorId}`;
  const title = isPending
    ? action === "pause"
      ? "Pausing orchestration"
      : action === "resume"
        ? "Resuming orchestration"
        : "Stopping orchestration"
    : action === "pause"
      ? "Pause orchestration"
      : action === "resume"
        ? "Resume orchestration"
        : "Stop orchestration";

  return (
    <RuntimeActionButton
      action={action}
      ariaLabel={label}
      title={title}
      classNamePrefix="session-orchestrator-group-action"
      isPending={isPending}
      disabled={disabled}
      onClick={onClick}
    />
  );
}

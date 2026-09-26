// Owns shared Kimi startup-mode and host-approval labels for settings and slash commands.
// Does not own provider admission or permission enforcement; those remain on the host.
import type { KimiApprovalMode, KimiMode } from "./types";

export const KIMI_APPROVAL_OPTIONS = [
  { value: "ask", label: "Ask", detail: "Show tool permission requests from Kimi" },
  { value: "auto-approve", label: "Auto-approve", detail: "Let TermAl approve Kimi tool requests; questions and plans remain interactive" },
] as const;

export const KIMI_MODE_OPTIONS = [
  { value: "default", label: "Default", detail: "Asks before commands and edits." },
  { value: "plan", label: "Plan", detail: "Start in plan mode." },
  { value: "yolo", label: "Ask when needed", detail: "Start in Ask When Needed mode: routine edits and commands run automatically; risky actions, questions, and plans still ask." },
  { value: "auto", label: "Never ask", detail: "Start in Never Ask mode: never interrupts you; everything runs and is decided automatically." },
] as const;

export function isKimiApprovalMode(value: string): value is KimiApprovalMode {
  return KIMI_APPROVAL_OPTIONS.some(option => option.value === value);
}

export function isKimiMode(value: string): value is KimiMode {
  return KIMI_MODE_OPTIONS.some(option => option.value === value);
}

export function kimiModeHint(mode: KimiMode) {
  const detail = KIMI_MODE_OPTIONS.find(option => option.value === mode)!.detail;
  return mode === "yolo" || mode === "auto"
    ? `${detail} Kimi rarely or never asks TermAl in this mode, so the host approval setting rarely applies.`
    : detail;
}

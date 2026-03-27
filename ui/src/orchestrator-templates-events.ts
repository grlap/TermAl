export const ORCHESTRATOR_TEMPLATES_CHANGED_EVENT = "termal-orchestrator-templates-changed";

export function dispatchOrchestratorTemplatesChangedEvent() {
  if (typeof window === "undefined") {
    return;
  }

  window.dispatchEvent(new CustomEvent(ORCHESTRATOR_TEMPLATES_CHANGED_EVENT));
}

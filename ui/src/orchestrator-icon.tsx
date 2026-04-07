import type { CSSProperties } from "react";
import orchestratorIconUrl from "./assets/cursors/orchestrator-svgrepo-com.svg";

export function OrchestratorIcon({ className = "" }: { className?: string }) {
  const nextClassName = className
    ? `orchestrator-icon ${className}`
    : "orchestrator-icon";

  return (
    <span
      className={nextClassName}
      aria-hidden="true"
      style={
        {
          "--orchestrator-icon-mask": `url("${orchestratorIconUrl}")`,
        } as CSSProperties
      }
    />
  );
}

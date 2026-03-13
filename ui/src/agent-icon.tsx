import type { CSSProperties } from "react";

import type { AgentType } from "./types";

const CLAUDE_SYMBOL_URL = "https://upload.wikimedia.org/wikipedia/commons/b/b0/Claude_AI_symbol.svg";
const OPENAI_LOGO_URL = "https://upload.wikimedia.org/wikipedia/commons/4/4d/OpenAI_Logo.svg";

export function AgentIcon({
  agent,
  className,
}: {
  agent: AgentType;
  className?: string;
}) {
  const classNames = ["agent-icon"];
  if (className) {
    classNames.push(className);
  }

  return (
    <span className={classNames.join(" ")} data-agent={agent.toLowerCase()} aria-hidden="true">
      {agent === "Codex" ? <OpenAiIcon /> : <ClaudeIcon />}
    </span>
  );
}

function OpenAiIcon() {
  return (
    <span
      className="agent-icon-symbol agent-icon-symbol-openai"
      style={{ "--agent-icon-mask": `url(${OPENAI_LOGO_URL})` } as CSSProperties}
    />
  );
}

function ClaudeIcon() {
  return (
    <span
      className="agent-icon-symbol agent-icon-symbol-claude"
      style={{ "--agent-icon-mask": `url(${CLAUDE_SYMBOL_URL})` } as CSSProperties}
    />
  );
}

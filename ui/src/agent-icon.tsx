import type { AgentType } from "./types";

const CLAUDE_SYMBOL_PATH =
  "M17.3041 3.541h-3.6718l6.696 16.918H24Zm-10.6082 0L0 20.459h3.7442l1.3693-3.5527h7.0052l1.3693 3.5528h3.7442L10.5363 3.5409Zm-.3712 10.2232 2.2914-5.9456 2.2914 5.9456Z";
const OPENAI_PETAL_ROTATIONS = [0, 60, 120, 180, 240, 300] as const;

export function AgentIcon({
  agent,
  className,
}: {
  agent: AgentType;
  className?: string;
}) {
  const classNames = ["agent-icon"];
  const agentLabel = agent;
  if (className) {
    classNames.push(className);
  }

  return (
    <span
      className={classNames.join(" ")}
      data-agent={agentLabel.toLowerCase()}
      aria-hidden="true"
    >
      {renderAgentIcon(agentLabel)}
    </span>
  );
}

function renderAgentIcon(agent: AgentType) {
  switch (agent) {
    case "Codex":
      return <OpenAiIcon />;
    case "Claude":
      return <ClaudeIcon />;
    case "Cursor":
      return <MonogramIcon label="C" />;
    case "Gemini":
      return <MonogramIcon label="G" />;
  }

  const unknownAgent: never = agent;
  return <MonogramIcon label={fallbackMonogram(String(unknownAgent))} />;
}

function fallbackMonogram(agent: string) {
  const label = agent.trim().charAt(0);
  return label ? label.toUpperCase() : "?";
}

function OpenAiIcon() {
  return (
    <svg
      className="agent-icon-symbol agent-icon-symbol-openai"
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {OPENAI_PETAL_ROTATIONS.map((rotation) => (
        <rect
          key={rotation}
          x="10.1"
          y="3.15"
          width="3.8"
          height="8.25"
          rx="1.9"
          transform={`rotate(${rotation} 12 12)`}
        />
      ))}
    </svg>
  );
}

function ClaudeIcon() {
  return (
    <svg
      className="agent-icon-symbol agent-icon-symbol-claude"
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <path fill="currentColor" d={CLAUDE_SYMBOL_PATH} />
    </svg>
  );
}

function MonogramIcon({ label }: { label: string }) {
  return <span className="agent-icon-monogram">{label}</span>;
}

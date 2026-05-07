import type {
  DelegationCommandResult,
  DelegationFinding,
  DelegationStatus,
} from "./types";

export type DelegationPromptResult = {
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
  findings?: DelegationFinding[];
  changedFiles?: string[];
  commandsRun?: DelegationCommandResult[];
  notes?: string[];
};

function normalizeMultiline(value: string, fallback: string) {
  const normalized = value.replace(/\r\n/g, "\n").replace(/\r/g, "\n").trim();
  return normalized.length > 0 ? normalized : fallback;
}

function indentContinuation(value: string, fallback: string) {
  return normalizeMultiline(value, fallback)
    .split("\n")
    .map((line, index) => (index === 0 ? line : `  ${line.trimEnd()}`))
    .join("\n");
}

function formatFindingLocation(finding: DelegationFinding) {
  const file = normalizeMultiline(finding.file ?? "", "");
  const line = typeof finding.line === "number" ? String(finding.line) : "";
  if (file && line) {
    return `${file}:${line}`;
  }
  return file || line;
}

function formatFinding(finding: DelegationFinding) {
  const severity = normalizeMultiline(finding.severity, "Note");
  const message = normalizeMultiline(finding.message, "No finding details.");
  const location = formatFindingLocation(finding);
  return formatListItem(
    `${severity}${location ? ` ${location}` : ""}: ${message}`,
    "Note: No finding details.",
  );
}

function formatListItem(value: string, fallback: string) {
  return `- ${indentContinuation(value, fallback)}`;
}

function formatCommand(command: DelegationCommandResult) {
  const commandText = indentContinuation(command.command, "unknown command");
  const status = indentContinuation(command.status, "unknown");
  return `- ${commandText}\n  Status: ${status}`;
}

export function formatDelegationResultPrompt(result: DelegationPromptResult) {
  const sections = [
    `Delegation result (${result.status}) from ${result.childSessionId}:`,
    "",
    normalizeMultiline(result.summary, "No summary provided."),
  ];
  const findings = result.findings ?? [];
  if (findings.length > 0) {
    sections.push("", "Findings:", ...findings.map(formatFinding));
  }
  const changedFiles = result.changedFiles ?? [];
  if (changedFiles.length > 0) {
    sections.push(
      "",
      "Changed files:",
      ...changedFiles.map((path) => formatListItem(path, "unknown path")),
    );
  }
  const commandsRun = result.commandsRun ?? [];
  if (commandsRun.length > 0) {
    sections.push("", "Commands run:", ...commandsRun.map(formatCommand));
  }
  const notes = result.notes ?? [];
  if (notes.length > 0) {
    sections.push(
      "",
      "Notes:",
      ...notes.map((note) => formatListItem(note, "No note details.")),
    );
  }
  return sections.join("\n");
}

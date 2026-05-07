import { describe, expect, it } from "vitest";

import { formatDelegationResultPrompt } from "./delegation-result-prompt";

describe("formatDelegationResultPrompt", () => {
  it("formats empty summaries with a fallback", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "  ",
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "No summary provided.",
      ].join("\n"),
    );
  });

  it("formats only the summary when optional sections are absent", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "Reviewed changes.",
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "Reviewed changes.",
      ].join("\n"),
    );
  });

  it("formats findings independently", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "Reviewed changes.",
        findings: [
          {
            severity: "High",
            file: "src/main.rs",
            line: 42,
            message: "Bug found",
          },
        ],
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "Reviewed changes.",
        "",
        "Findings:",
        "- High src/main.rs:42: Bug found",
      ].join("\n"),
    );
  });

  it("formats changed files independently", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "Reviewed changes.",
        changedFiles: ["src/main.rs"],
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "Reviewed changes.",
        "",
        "Changed files:",
        "- src/main.rs",
      ].join("\n"),
    );
  });

  it("formats command results independently", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "Reviewed changes.",
        commandsRun: [{ command: "cargo check", status: "success" }],
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "Reviewed changes.",
        "",
        "Commands run:",
        "- cargo check",
        "  Status: success",
      ].join("\n"),
    );
  });

  it("formats notes independently", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "Reviewed changes.",
        notes: ["Needs follow-up"],
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "Reviewed changes.",
        "",
        "Notes:",
        "- Needs follow-up",
      ].join("\n"),
    );
  });

  it("formats all optional result sections", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "failed",
        summary: "Reviewed changes.",
        findings: [
          {
            severity: "High",
            file: "src/main.rs",
            line: 42,
            message: "Bug found",
          },
          {
            severity: "Low",
            file: "README.md",
            message: "Docs gap",
          },
          {
            severity: "Note",
            line: 7,
            message: "Line-only context",
          },
          {
            severity: "",
            message: "",
          },
        ],
        changedFiles: ["src/main.rs"],
        commandsRun: [{ command: "cargo check", status: "success" }],
        notes: ["Needs follow-up"],
      }),
    ).toBe(
      [
        "Delegation result (failed) from child-1:",
        "",
        "Reviewed changes.",
        "",
        "Findings:",
        "- High src/main.rs:42: Bug found",
        "- Low README.md: Docs gap",
        "- Note 7: Line-only context",
        "- Note: No finding details.",
        "",
        "Changed files:",
        "- src/main.rs",
        "",
        "Commands run:",
        "- cargo check",
        "  Status: success",
        "",
        "Notes:",
        "- Needs follow-up",
      ].join("\n"),
    );
  });

  it("indents multiline values so markdown bullets stay stable", () => {
    expect(
      formatDelegationResultPrompt({
        childSessionId: "child-1",
        status: "completed",
        summary: "Line one\nLine two",
        findings: [
          {
            severity: "Medium",
            file: "src/file.rs\nextra",
            message: "- starts like a bullet\nsecond line",
          },
        ],
        changedFiles: ["path/one\npath/two"],
        commandsRun: [{ command: "npm test\n-- --run", status: "error\n1" }],
        notes: ["note one\n- nested-looking line"],
      }),
    ).toBe(
      [
        "Delegation result (completed) from child-1:",
        "",
        "Line one",
        "Line two",
        "",
        "Findings:",
        "- Medium src/file.rs",
        "  extra: - starts like a bullet",
        "  second line",
        "",
        "Changed files:",
        "- path/one",
        "  path/two",
        "",
        "Commands run:",
        "- npm test",
        "  -- --run",
        "  Status: error",
        "  1",
        "",
        "Notes:",
        "- note one",
        "  - nested-looking line",
      ].join("\n"),
    );
  });
});

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { AgentIcon } from "./agent-icon";

describe("AgentIcon", () => {
  it("renders bundled svg marks and local monograms without remote icon URLs", () => {
    const { container } = render(
      <>
        <AgentIcon agent="Codex" />
        <AgentIcon agent="Claude" />
        <AgentIcon agent="Cursor" />
        <AgentIcon agent="Gemini" />
      </>,
    );

    const codexMark = container.querySelector(
      '.agent-icon[data-agent="codex"] .agent-icon-symbol-openai',
    );
    const claudeMark = container.querySelector(
      '.agent-icon[data-agent="claude"] .agent-icon-symbol-claude',
    );
    const cursorMonogram = container.querySelector(
      '.agent-icon[data-agent="cursor"] .agent-icon-monogram',
    );
    const geminiMonogram = container.querySelector(
      '.agent-icon[data-agent="gemini"] .agent-icon-monogram',
    );

    expect(codexMark?.tagName.toLowerCase()).toBe("svg");
    expect(claudeMark?.tagName.toLowerCase()).toBe("svg");
    expect(cursorMonogram?.textContent).toBe("C");
    expect(geminiMonogram?.textContent).toBe("G");
    expect(codexMark).not.toHaveAttribute("style");
    expect(claudeMark).not.toHaveAttribute("style");
    expect(container.innerHTML).not.toContain("upload.wikimedia.org");
    expect(container.innerHTML).not.toContain("--agent-icon-mask");
  });
  it("renders a graceful monogram fallback for unknown agents", () => {
    const { container } = render(<AgentIcon agent={"UnknownBot" as any} />);

    const fallbackMonogram = container.querySelector(
      '.agent-icon[data-agent="unknownbot"] .agent-icon-monogram',
    );

    expect(fallbackMonogram?.textContent).toBe("U");
  });

});

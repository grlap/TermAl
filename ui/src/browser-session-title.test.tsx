import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  DEFAULT_BROWSER_TITLE,
  useLastActiveSessionDocumentTitle,
} from "./browser-session-title";

function BrowserTitleHarness({
  activeSessionName,
  workspaceLabel,
}: {
  activeSessionName: string | null;
  workspaceLabel?: string;
}) {
  useLastActiveSessionDocumentTitle(activeSessionName, workspaceLabel);
  return null;
}

afterEach(() => {
  cleanup();
  document.title = DEFAULT_BROWSER_TITLE;
});

describe("last active session browser title", () => {
  it("shows the workspace label and updates it without losing the last session title", () => {
    const view = render(<BrowserTitleHarness activeSessionName={null} workspaceLabel="Backend" />);
    expect(document.title).toBe("Backend · TermAl");
    view.rerender(<BrowserTitleHarness activeSessionName="API review" workspaceLabel="Backend" />);
    expect(document.title).toBe("Backend · API review · TermAl");
    view.rerender(<BrowserTitleHarness activeSessionName={null} workspaceLabel="Planning" />);
    expect(document.title).toBe("Planning · API review · TermAl");
    view.rerender(<BrowserTitleHarness activeSessionName={null} workspaceLabel="" />);
    expect(document.title).toBe("API review · TermAl");
  });
  it("keeps the last active session name while non-session tabs are active", () => {
    const view = render(<BrowserTitleHarness activeSessionName={null} />);
    expect(document.title).toBe("TermAl");

    view.rerender(<BrowserTitleHarness activeSessionName="  API review  " />);
    expect(document.title).toBe("API review · TermAl");

    view.rerender(<BrowserTitleHarness activeSessionName={null} />);
    expect(document.title).toBe("API review · TermAl");

    view.rerender(<BrowserTitleHarness activeSessionName="Frontend" />);
    expect(document.title).toBe("Frontend · TermAl");
  });
});

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  DEFAULT_BROWSER_TITLE,
  useLastActiveSessionDocumentTitle,
} from "./browser-session-title";

function BrowserTitleHarness({
  activeSessionName,
}: {
  activeSessionName: string | null;
}) {
  useLastActiveSessionDocumentTitle(activeSessionName);
  return null;
}

afterEach(() => {
  cleanup();
  document.title = DEFAULT_BROWSER_TITLE;
});

describe("last active session browser title", () => {
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

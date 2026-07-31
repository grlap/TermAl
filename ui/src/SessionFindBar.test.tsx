import { render, screen } from "@testing-library/react";
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";

import { SessionFindBar } from "./SessionFindBar";

describe("SessionFindBar", () => {
  it("labels results as partial when older transcript pages are not resident", () => {
    render(
      <SessionFindBar
        inputRef={createRef<HTMLInputElement>()}
        query="needle"
        activeIndex={0}
        matches={[
          {
            itemId: "message-20",
            itemKey: "message:message-20",
            itemKind: "message",
            snippet: "needle",
          },
        ]}
        isPartial
        onChange={vi.fn()}
        onNext={vi.fn()}
        onPrevious={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByText("1 of 1 loaded")).toBeInTheDocument();
    expect(
      screen.getByTitle(
        "Search covers loaded messages only; older or newer history has not been searched.",
      ),
    ).toBeInTheDocument();
  });

  it("does not qualify results when the complete transcript is resident", () => {
    render(
      <SessionFindBar
        inputRef={createRef<HTMLInputElement>()}
        query="needle"
        activeIndex={-1}
        matches={[]}
        isPartial={false}
        onChange={vi.fn()}
        onNext={vi.fn()}
        onPrevious={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByText("No matches")).toBeInTheDocument();
  });
});

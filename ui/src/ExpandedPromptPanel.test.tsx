import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ExpandedPromptPanel } from "./ExpandedPromptPanel";

describe("ExpandedPromptPanel", () => {
  it("reveals the expanded prompt on demand", () => {
    render(<ExpandedPromptPanel expandedText={"Review staged changes in detail."} />);

    expect(screen.queryByText("Expanded prompt")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show expanded prompt" }));

    expect(screen.getByText("Expanded prompt")).toBeInTheDocument();
    expect(screen.getByText("Review staged changes in detail.")).toBeInTheDocument();
  });

  it("restores the expanded state across remounts for the same storage key", () => {
    const { unmount } = render(
      <ExpandedPromptPanel
        expandedText={"Review staged changes in detail."}
        storageKey="expanded-prompt-test-remount"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Show expanded prompt" }));
    expect(screen.getByText("Expanded prompt")).toBeInTheDocument();

    unmount();

    render(
      <ExpandedPromptPanel
        expandedText={"Review staged changes in detail."}
        storageKey="expanded-prompt-test-remount"
      />,
    );

    expect(screen.getByText("Expanded prompt")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hide expanded prompt" })).toBeInTheDocument();
  });
});

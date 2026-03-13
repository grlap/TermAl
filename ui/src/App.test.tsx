import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MarkdownContent } from "./App";

describe("MarkdownContent", () => {
  it("wraps markdown tables in a scroll container", () => {
    const markdown = [
      "| Finding | Resolution |",
      "| --- | --- |",
      "| `skip_list.rs` | Fixed |",
    ].join("\n");

    const { container } = render(<MarkdownContent markdown={markdown} />);

    const tableScroll = container.querySelector(".markdown-table-scroll");
    expect(tableScroll).not.toBeNull();
    expect(tableScroll?.querySelector("table")).not.toBeNull();
  });
});

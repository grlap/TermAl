import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { GitStatusTreeRows, type GitStatusTreeOptions } from "./GitStatusTreeRows";
import { buildGitPathTree, buildGitStatusTree, type GitStatusTreeDirectoryNode, type GitStatusTreeFileNode } from "./git-status-tree";

describe("GitStatusTreeRows", () => {
  it("leaves an unchanged ordinary branch alone when a sibling changes", () => {
    const before = buildGitStatusTree([
      { path: "alpha/steady.ts", worktreeStatus: "M" },
      { path: "beta/old.ts", worktreeStatus: "M" },
    ]);
    const after = buildGitStatusTree([
      { path: "alpha/steady.ts", worktreeStatus: "M" },
      { path: "beta/new.ts", worktreeStatus: "M" },
    ], before);
    const directoryActions = vi.fn((_node: GitStatusTreeDirectoryNode) => ({ actions: null, isPending: false }));
    const options: GitStatusTreeOptions<GitStatusTreeFileNode> = {
      directoryKey: (path) => path,
      onTreeToggle: () => {},
      treeExpansionByKey: {},
      directoryActions,
      renderFile: (node) => <span>{node.name}</span>,
    };
    const { rerender } = render(<GitStatusTreeRows nodes={before[1].nodes} options={options} />);
    expect(directoryActions.mock.calls.map(([node]) => node.path)).toEqual(["alpha", "beta"]);
    directoryActions.mockClear();

    rerender(<GitStatusTreeRows nodes={after[1].nodes} options={options} />);

    expect(screen.getByText("steady.ts")).toBeInTheDocument();
    expect(screen.getByText("new.ts")).toBeInTheDocument();
    expect(screen.queryByText("old.ts")).not.toBeInTheDocument();
    expect(directoryActions.mock.calls.map(([node]) => node.path)).toEqual(["beta"]);
  });

  it("exposes the full relative paths on same-named submodule folders", () => {
    render(
      <GitStatusTreeRows
        nodes={buildGitPathTree(["one/shared/a.ts", "two/shared/b.ts"])}
        options={{
          directoryKey: (path) => path,
          onTreeToggle: () => {},
          treeExpansionByKey: {},
          showFolderPaths: true,
          renderFile: (node) => <span>{node.name}</span>,
        }}
      />,
    );

    const [one, two] = screen.getAllByRole("button", { name: "shared" });
    expect(one).toHaveAccessibleDescription("one/shared");
    expect(one).toHaveAttribute("title", "one/shared");
    expect(two).toHaveAccessibleDescription("two/shared");
    expect(two).toHaveAttribute("title", "two/shared");
  });
});

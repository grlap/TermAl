// Owns the submodule marker and read-only file groups for GitStatusPanel.
// Git requests and action handlers stay in GitStatusPanel.

import { useMemo } from "react";
import type { GitSubmoduleFiles } from "../api";
import { GitStatusTreeName, GitStatusTreeRows, type GitStatusTreeExpansion, type GitStatusTreeOptions } from "./GitStatusTreeRows";
import { buildGitPathTree, type GitPathTreeFileNode, type GitStatusTreeSubmodule } from "./git-status-tree";

export function GitStatusSubmoduleLabels({ id, submodule }: { id: string; submodule: GitStatusTreeSubmodule }) {
  return (
    <span id={id} className="git-status-submodule-labels">
      {/* CSS gaps do not separate the words in the accessible description. */}
      <span className="git-status-submodule-marker">Submodule</span>{" "}
      {submodule.commits ? <span>Pointer changed</span> : null}{" "}
      {submodule.modified ? <span>Modified</span> : null}{" "}
      {submodule.untracked ? <span>Untracked</span> : null}
    </span>
  );
}

type SubmoduleTreeExpansion = GitStatusTreeExpansion & { expansionKey: string };

export function GitStatusSubmoduleFiles({ submodule, ...expansion }: SubmoduleTreeExpansion & {
  submodule: GitStatusTreeSubmodule;
}) {
  const { checkout, commits, modified, untracked } = submodule;
  if (checkout === "notCheckedOut") {
    return <p className="git-status-submodule-message">Not checked out.</p>;
  }
  if (checkout === "unavailable" || (!commits && !modified && !untracked)) {
    return <p className="git-status-submodule-message">File details are unavailable for this change.</p>;
  }
  return (
    <>
      {commits ? (
        <FileGroup
          {...expansion}
          label="Commits"
          files={commits}
          empty="The pointer changed, but Git listed no changed paths."
        />
      ) : null}
      {modified ? <FileGroup {...expansion} label="Modified" files={modified} /> : null}
      {untracked ? <FileGroup {...expansion} label="Untracked" files={untracked} /> : null}
    </>
  );
}

function FileGroup({
  label,
  files,
  empty = "Git reported this change without file paths.",
  ...expansion
}: SubmoduleTreeExpansion & {
  label: string;
  files: GitSubmoduleFiles;
  empty?: string;
}) {
  return (
    <div className="git-status-submodule-group" role="group" aria-label={label}>
      <span className="git-status-submodule-group-label">{label}</span>
      {files.state === "listed" ? (
        <>
          {files.paths.length > 0 ? (
            <>
              <SubmodulePathTree {...expansion} label={label} paths={files.paths} />
              {files.remaining > 0 ? <p className="git-status-submodule-message">+{files.remaining} more</p> : null}
            </>
          ) : <p className="git-status-submodule-message">{empty}</p>}
          {files.incomplete ? <p className="git-status-submodule-message">Some nested submodule files could not be read.</p> : null}
        </>
      ) : (
        <p className="git-status-submodule-message">
          {files.state === "missingCommit" ? (
            <>Commit <code>{files.commit}</code> is not present locally.</>
          ) : "File details could not be read."}
        </p>
      )}
    </div>
  );
}

function SubmodulePathTree({ expansionKey, label, paths, onTreeToggle, treeExpansionByKey }: SubmoduleTreeExpansion & {
  label: string;
  paths: string[];
}) {
  const nodes = useMemo(() => buildGitPathTree(paths), [paths]);
  const options = useMemo<GitStatusTreeOptions<GitPathTreeFileNode>>(() => ({
    asList: true,
    directoryKey: (path) => `submodule-directory:${JSON.stringify([expansionKey, label, path])}`,
    onTreeToggle,
    treeExpansionByKey,
    showFolderPaths: true,
    renderFile: (node) => (
      <div className="git-status-tree-row git-status-tree-file-row">
        <span className="git-status-tree-open-button">
          <span className="git-tree-toggle git-tree-toggle-placeholder" aria-hidden="true" />
          <GitStatusTreeName ariaHidden name={node.name} title={node.path} />
          <span className="visually-hidden">{node.path}</span>
        </span>
      </div>
    ),
  }), [expansionKey, label, onTreeToggle, treeExpansionByKey]);

  return <GitStatusTreeRows nodes={nodes} options={options} />;
}

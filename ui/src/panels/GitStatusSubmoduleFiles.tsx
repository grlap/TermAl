// Owns the submodule marker and read-only file groups for GitStatusPanel.
// Row expansion, Git requests, and action handlers stay in GitStatusPanel.

import type { GitSubmoduleFiles } from "../api";
import type { GitStatusTreeSubmodule } from "./git-status-tree";

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

export function GitStatusSubmoduleFiles({ submodule }: { submodule: GitStatusTreeSubmodule }) {
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
        <FileGroup label="Commits" files={commits} empty="The pointer changed, but Git listed no changed paths." />
      ) : null}
      {modified ? <FileGroup label="Modified" files={modified} /> : null}
      {untracked ? <FileGroup label="Untracked" files={untracked} /> : null}
    </>
  );
}

function FileGroup({
  label,
  files,
  empty = "Git reported this change without file paths.",
}: {
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
            <ul className="git-status-submodule-paths">
              {files.paths.map((path, index) => (
                <li key={index}>{path}</li>
              ))}
              {files.remaining > 0 ? <li className="git-status-submodule-message">+{files.remaining} more</li> : null}
            </ul>
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

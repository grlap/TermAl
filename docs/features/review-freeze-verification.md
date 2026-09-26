# Host-owned read-only review verification

This capability belongs to the [delegation review contract](agent-delegation-sessions.md)
and [backend architecture](../architecture.md). It supplies a bounded independent
check for repositories using Engram's schema-1 freeze algorithm. It does not
change Claude's ask-all permission overlay or allow Node, shell scripts, builds,
tracker operations, or arbitrary programs in read-only children.

## Contract

An active local `reviewer` with `writePolicy: readOnly` calls
`termal_review_freeze_check` with:

```json
{
  "manifestPath": ".git/engram-review-freeze.json",
  "expectedFingerprint": "<64 lowercase hex characters recorded separately by the parent>"
}
```

Call before and after source inspection. Read the expected literal from the
parent's frozen task context, not from the manifest at verification time.
The backend derives the worktree from the current linked child and checks the
attempt again after completion. Caller-supplied cwd, commands, executables,
environment, and script text are rejected. The manifest must resolve inside
the admitted worktree, or inside that worktree's own Git directory
(`git rev-parse --absolute-git-dir`, read with the checker's pinned Git
configuration). That second place is where `git rev-parse --git-path
engram-review-freeze.json` resolves: `<root>/.git` for a main worktree, which
is already inside the root, and `<common>/worktrees/<name>` for a linked one,
which is not. It keeps the manifest out of review input. For a linked
worktree's review, the shared common Git directory and another worktree's Git
directory are refused. For a main worktree's review they lie under the root
and are accepted, as before. The Git directory is the one the worktree's
`.git` file names, which the checker trusts as every Git command does. Where
the manifest lives never decides the result: its fingerprint must equal the
parent's independently supplied literal.
The delegation cwd must be the Git worktree root, not a repository subdirectory.
Delegate at the root for this capability; a subdirectory delegation cannot
successfully use this initial freeze profile.

The host invokes its own executable in `review-freeze-check ROOT MANIFEST HASH`
mode, which never starts a server or opens TermAl stores. A real observer
captures status, signal, error, stdoutLength/stderrLength, stdoutBase64 and
stderrBase64. `verified` and `observer.stdoutExact` are true only for exit 0
and stdout **exactly** the independently supplied hash plus LF (65 bytes).
Stderr remains separate; the Windows executable-mode/symlink limitation is
reported there. Nonzero exit, malformed manifest, drift, root mismatch, extra
stdout, timeout, and unsupported inputs cannot become a clean verification.
An observed failed check returns HTTP 200 with `verified: false` and MCP
`isError: true`; transport failures use HTTP 500. Request/role/capacity errors
are documented in the architecture endpoint table. Only two checks run at once.
Capacity exhaustion returns HTTP 429: retry the same read. HTTP 409 is reserved
for reviewer authority or attempt conflicts, not a busy checker.

## Algorithm and safety boundaries

`engram-review-freeze-v1` matches schema 1's SHA-256 chunk framing: unsigned
big-endian 64-bit label length and payload length, then UTF-8 label and raw
payload. Chunks are `schema=1`, `head=HEAD|UNBORN`, staged binary/full-index
diff, unstaged binary/full-index diff, then byte-sorted untracked paths with
mode/content or symlink target. This is not the different fingerprint format
used by TermAl's own `scripts/review-freeze-fingerprint.mjs`.

The compiled checker does not load repository code. Git is resolved to an
absolute PATH executable outside the target root, with no shell/current-directory
executable search. Hooks, fsmonitor, paging, external diff and textconv are
disabled. Lazy fetching is disabled (`GIT_NO_LAZY_FETCH=1`) and all Git
transports are denied (`GIT_ALLOW_PROTOCOL` is empty). Missing promisor objects
fail locally instead of downloading objects or invoking repository-configured
transport helpers. Repositories with clean/smudge/process filters are rejected.
Gitlinks in HEAD or the index (including newly staged or removed submodules)
are unsupported and rejected before diffing: a submodule has its own filter
configuration. Diffs also disable submodule inspection as defense in depth;
ordinary repositories retain the Engram schema-1 fingerprint framing. Global
and system Git config and inherited `GIT_*` overrides are excluded; if the
parent produced its manifest using materially different Git settings, the check
fails rather than silently accepting a different algorithm. Re-freezing needs
the parent's normal workflow, not a reviewer write exception.

On Windows, a CRLF worktree whose parent relies on global/system
`core.autocrlf=true` can produce a different unstaged diff under the isolated
checker. Pin the intended normalization in repository-local Git configuration
or tracked attributes before the parent freezes, then verify compatibility.
Global excludes and ownership exceptions are not imported either; a mismatch
is a failed verification, never permission to execute global Git configuration.
Default XDG/HOME ignore and attributes files are disabled with explicit null
`core.excludesFile`/`core.attributesFile`; system attributes are disabled with
`GIT_ATTR_NOSYSTEM=1`. Repository `.gitignore`/`.gitattributes` and local
normalization remain effective. Engram's reference script inherits its caller's
Git environment: schema-1 framing compatibility does not imply equal hashes
under different ignore/attribute rules. The parent must freeze under equivalent
rules; the checker never imports unsafe configuration to obtain a match.
Embedded untracked Git repositories are unsupported: directory entries are
rejected explicitly, never recursively inspected.

The compact `/api/state` delegation summary includes `reviewFreezeAllowed`,
a static policy capability true only for read-only reviewers. The MCP bridge
uses it for both discovery and calls; older payloads without it fail closed.
This is not a live authorization receipt: the endpoint still checks the current
child, worktree, running attempt and policy before and after the subprocess.

The check bounds process time (20 seconds internally, 25 seconds for the
observer), output (128 MiB per Git stdout, 64 KiB retained stderr, 4 KiB checker stdout),
manifest (1 MiB), and untracked input (20,000 entries, 128 MiB/file, 512 MiB total).
Excess stderr is drained and discarded; Git failures embed at most 4 KiB of
diagnostics so ordinary verification failure remains an observed failed check.
Owned Unix process groups are terminated before reaping the leader; Windows
Job Objects retain descendants through pipe collection. After exit, pipe
collection has one shared one-second grace, including when exit was observed
just past the process deadline; it never waits indefinitely for reader threads.
Unsafe paths, non-UTF8 names/targets and special filesystem objects fail closed.
Two captures, file metadata checks, and a manifest re-read reject observed
concurrent changes; they are not an atomic filesystem snapshot or proof that
no transient edit occurred between observations. Windows untracked executable
bits and filesystem symlink properties remain unverified, matching Engram's
declared limitation.

## Activation and acceptance

Both the host and the child delegation MCP bridge need the new executable.
An old host still denies the new tool; no generic interpreter fallback exists.
Do not restart an active host as part of review. Prepare the artifact, then let
the operator activate it at an agreed boundary and start a fresh Claude reviewer
so the tool definition and host-injected guidance are refreshed.

Each verification resolves `current_exe()` and launches the executable then
available at that path, not a retained copy of the running host's bytes. Replacing
the binary in place can therefore select new checker bytes on macOS or make the
deleted executable path unlaunchable on Linux (a fail-closed HTTP 500). Record
the activated artifact and avoid replacing it during acceptance.

Acceptance requires a real Claude read-only review using the supported check
before and after inspection, plus real denied-write/interpreter witnesses.
Unit tests, a standalone checker match and parent quality gates alone do not
complete that acceptance. Keep the implementation task open until those results
are recorded. Repository review commands must accept this host-owned equivalent;
the tool does not authorize ignoring a conflicting mandatory repository check.

Automated coverage separates two boundaries: the Cargo integration test invokes
the actual compiled checker (exact stdout, failure, drift and hostile XDG), while
HTTP integration tests use a real bounded fixture process through an internal
executor seam to assert `200/verified:false`, success, and a channel-gated attempt
change rejected with 409. The fixture is not the checker binary or live host;
it cannot be selected by an HTTP/MCP caller. Neither test replaces the real
Claude acceptance above.

# Source identity v1

Status: replacement contract frozen for exact-wording review. The agreed direction is
one general git-commit test-credit lane for Git repositories, with or without
gitlinks. Commit-input v1 names its versioned eligibility rules. Content-v1
retains its hash format and gains the spelling-admission change below.
The adopted Git collision observations appear in the conformance cases below.
TermAl test credit is the first implementation; later consumers must
migrate explicitly. This document does not claim current implementation coverage.
TermAl owns this canonical document together with Engram. Engram links to it
rather than maintaining a second copy.

`source_id` is the field name. The content basis is `content-v1:<digest>`;
the general committed-input lane uses `git-commit:<full commit id>` of the root
repository, with its tree recorded separately. Gitlinks add child eligibility
checks to that lane; Phoenix and Questly are examples, not its scope. These
are distinct schemes, not interchangeable labels.

No source identity enforces the on-disk case spelling of paths. In content-v1,
a spelling-only collision of two tracked paths onto one file is an aliasing
refusal, not a spelling requirement. The committed-input lane instead uses
Git's cleanliness verdict, including the equal-blob case described below.

## Purpose and rollout

One `capture_source` / `verify_source` contract identifies supported source
input and returns the admission evidence for its scheme. Its consumers migrate
in this order:

1. TermAl test-credit source capture, including pinned submodules.
2. TermAl `review_freeze` and `review_identity`, through an explicit consumer
   migration that preserves their additional evidence and checks.
3. Engram's Node freeze and launcher tooling, after its current task-sync work.

TermAl's compiled capture is the initial reference implementation. Engram may
eventually call a trusted installed executable or link a shared implementation;
that packaging choice remains open because Engram also runs without a TermAl
binary. There is one scheme and admission contract, not separately evolving
Rust and Node identity algorithms. Any Engram implementation must be tested
against TermAl's capture on the same fixtures.

A source identity is not a run, record, review, evaluation or build identity. It
does not prove that a command ran, that it passed, that a root is authorized, or
that a claim or fence is current. Those facts retain their own evidence.

## Capture and verification

The names below describe the logical API, not a newly shipped CLI or wire format:

```text
capture_source(explicit_local_worktree_root, requested_basis)
    -> Captured { source_id, admission_evidence, local_capture_provenance }
     | Error { reason, optional_relative_path }

verify_source(explicit_local_worktree_root, expected_source_id)
    -> Match { captured_source_id }
     | Mismatch { captured_source_id }
     | Error { reason, optional_relative_path }
```

The consumer explicitly selects `requested_basis`: `content-v1` or
`commit-input v1`. Capture applies only that basis and never falls back to the
other one on refusal. This API does not change any consumer's default.
Verification validates the expected scheme and value, derives the requested
basis from that scheme (`content-v1:` or `git-commit:`), then performs the same
complete capture without fallback. Only a successful supported capture can yield
match or mismatch. Unavailable files, unknown schemes, unsupported states, cancellation,
races, deadlines and failed reads yield errors. An error is never an empty
identity, an invented mismatch, or permission to reuse an earlier match.

For commit-input v1, admission evidence includes the root commit and tree,
child pins and checked HEADs, materialization, and the cleanliness result at
that boundary. Failure to establish eligibility
returns a named reason and makes the run ineligible for credit. Knowing a Git
commit id alone is not a successful eligible capture.

The API needs an explicit local root, the requested basis (or expected identity
for verification), and a trusted installed producer. It needs no project store,
configured home, MCP session, network or write permission.
Read-only reviewers must not execute a helper supplied by the reviewed worktree.
The root is canonicalized and verified as the Git worktree root. Its absolute
location belongs only to local provenance, never to the digest.

Capture does not fetch, initialize or repair a submodule; run hooks, filters or
external diff programs; write objects or an index; stage files; or modify source
or configuration. Pin the Git executable outside the inspected repository and
retain the existing bounded Git runner, disabled optional locks, disabled
fsmonitor, disabled global/system ignore and attribute files, disabled hooks,
filter refusal, and denied transports/lazy fetching. In committed-input capture,
disable replacement objects for every Git operation in the root and children,
including status and commit, tree and pin reads. Local settings may not silently
weaken child cleanliness checks.

For committed-input captures, compare the root commit, tree and every child pin at both
run boundaries; root HEAD and each initialized child's HEAD must remain
unchanged. Re-establish eligibility at each boundary.
Observed drift during a capture refuses its result. Content-v1 captures retain
their existing race checks and gain only the admission change below, whose
one new refusal is the spelling-only collision; this rollout adds no other
refusal to that path. Their per-file reads retain checked
metadata/handle reads.
Equal boundary observations do not prove an atomic filesystem snapshot or
exclude an undetected change-and-revert between observations. Consumers still
need their existing before/after execution and mutation rules.

## Ordinary content: preserve content-v1

The compatibility reference is `src/content_revision.rs` at commit
`cd79424e8b2d1fa453ab43abb23761152a0d8913`, using `review_freeze_chunk` and the checked file reader in
`src/review_freeze.rs`. Content-v1 keeps exactly the existing byte encoding
and values for input admitted by both the old and new rules. The admission
change below changes no hash input or scheme. Do not prepend a new global object-format
field, change the schema chunk, or rename stored values to `source_id-v1`.

Coverage is the current worktree content of all paths listed by the pinned Git
invocation as tracked, plus untracked non-ignored paths. This is not merely a
tracked-files hash. Ordinary ignored untracked files remain outside coverage.
A force-added ignored file is included while tracked; removing it from the
index can remove it from coverage. A consumer needing a clean worktree must
also apply its existing cleanliness rules.

The pinned Git invocation still reads current worktree `.gitignore` files and
repository-local `.git/info/exclude`. The latter is not source content and can
change the admitted untracked set between roots. This is preserved compatibility,
not a guarantee that two roots with different local exclusions admit the same
paths.

HEAD and index file contents or modes are not hashed. Staging or committing
unchanged admitted content does not move the identity. The index still selects
tracked membership, and Git supplies the existing attribute classification.
This is the precise meaning of HEAD-blind and index-blind ordinary content;
it is not a promise to ignore the index when admitting paths or dependency pins.

For this no-gitlink content-v1 scheme, the same admitted content at a different
HEAD or index yields the same source_id. This does not apply to git-commit
identity. Candidate review, gates and landing must continue to bind the exact
commit and tree and their own authority evidence; source_id is never their only
evidence. Ordinary admitted content includes path membership and actual mode.

For ordinary entries:

- Use repository-relative UTF-8 Git paths, sorted by their original byte
  sequence and deduplicated by exact path. No case folding, Unicode
  normalization, platform separator rewrite or absolute-root prefix.
- A missing tracked file contributes no entry, as after committed deletion.
  A tracked file replaced by a directory contributes no file entry; its
  separately listed non-ignored children contribute normally.
- A missing tracked `.gitattributes` refuses capture, because Git could then
  source its attributes from the index.
- Regular files contribute their content and actual worktree executable mode:
  `100755` for any Unix executable bit, otherwise `100644`. Windows retains the
  existing `100644` treatment. An index-only chmod does not change this.
- A final ordinary symlink contributes link text and mode `120000`, without
  following its target. Its target text must be UTF-8 and its entry must remain
  stable during the read. A redirecting ancestor is refused.
- Preserve the existing line-ending algorithm exactly. Explicit `eol=` or
  `-text` attributes, and executable files on Unix, preserve bytes. Other files
  use `content_revision_line_endings`: binary detection includes lone CR, NUL
  and the existing printable/nonprintable-byte rule; nonbinary CRLF text is
  normalized to LF. It does not consult the index blob. Do not replace this
  with a new interpretation of `core.autocrlf` or invoke a clean filter.
- Unsafe paths, unsupported encodings, unreadable or unsupported entries,
  and untracked nested repositories refuse
  capture. Existing platform path rules remain: colon and backslash are refused
  on Windows, not newly prohibited in ordinary Unix filenames.

Content-v1 admits a directory whose on-disk case spelling differs from the Git
listing. The reported F# paths
`tests/FSharp.Compiler.ComponentTests/CompilerOptions/Fsc` and
`tests/FSharp.Compiler.ComponentTests/CompilerOptions/fsc` may traverse one
on-disk directory; differing directory spelling alone is not a refusal. For
case or Unicode-normalization collisions, refuse only when two distinct
tracked paths that differ only by case and/or NFC/NFD normalization resolve
to one file on disk, naming both paths. Distinct-name hard links and terminal
symlink treatment remain unchanged. A parent link or junction leading elsewhere
is still refused. Hash the existing Git-listed paths and content without
rewriting their spelling.

This is an admission change, not a format change. It newly admits
directory-spelling differences, and its only new refusal is the spelling-only
collision above (two tracked paths differing only by case and/or NFC/NFD that
resolve to one file), which today's code admits, although such a checkout
cannot faithfully hold both committed paths. Values of every input admitted
by both remain unchanged. Apart from this change, the ordinary path retains its
admission behavior and limits. In
particular, it does not add root-wide sparse-checkout, unresolved
index, hidden-index-flag or HEAD/index-race checks to that path. Its evidence
still describes the admitted worktree content, not certification of a complete
checkout. The stricter commit-input rules below apply when selecting the
committed-input lane, with or without gitlinks, not to this content-v1 path.
Any future root-wide tightening needs a separately justified behavior change
and explicit consumer migration; it is not folded into byte compatibility.

These legacy states retain their existing admission behavior, without a new
v1 completeness guarantee: sparse checkouts, skip-worktree or assume-unchanged
index flags, a conflicted index, and changes between reads. This list does not
claim that every such fixture succeeds; existing refusals still apply.

## General committed input: commit-input v1

The git-commit test-credit lane applies to every Git repository, with or
without gitlinks. Commit-input v1 versions only the eligibility rules of that
lane. Gitlinks add the requirement that every child is initialized at its pin
and Git-clean. The original content-v1 producer
refused gitlinks; support uses the existing typed Git form, not new content-v1
chunks:

```text
source_id = "git-commit:" + full_root_commit_id
recorded_tree = tree_of_that_commit
```

The root commit identifies its tree and the pins recorded in that tree. Use
full object ids validated against the repository's declared object format.
The existing registered Commit lane admits only SHA-1 root repositories today:
40 lowercase hex digits. A SHA-256 root repository (including a superproject)
is refused with a named unsupported-format reason for commit-input credit until that lane is explicitly
widened. Never truncate, rehash or silently accept its 64-digit id. Preserve
format provenance; do not guess it from a permissive length check. Use the
existing typed Git-input representation rather than introducing a second wire
contract. Child object format and pin validation must agree with that basis.

Two commits with equal trees are not interchangeable. A new commit changes this
identity even if its tree is unchanged. This lane requires clean committed
input even when there are no gitlinks. `git-tree:` remains reserved for the
staged lane, which commit-input v1 does not offer for gitlink repositories.
Dirty or staged gitlink input gets no credit through a fallback content hash.
Commit-input v1 compares the gitlink pins in the HEAD tree with those in the
index; index-only additions, removals or pin changes make that lane ineligible,
and it never falls back to content-v1. An explicitly requested content-v1
capture retains today's index-based gitlink detection and existing behavior;
this adds no HEAD-tree check or second new refusal to content-v1. A commit-input
capture without a resolvable root commit is ineligible.

### Eligibility at both boundaries

Judge cleanliness through trusted Git using each repository's own
`core.ignoreCase` and `core.fileMode`. Leave both settings unpinned; do not force
either value to construct a different filesystem-spelling judgment. Retain the
read-only runner restrictions above. On every cleanliness status call, in the
root and in each child, pin these settings for that invocation without writing
repository or global configuration:

| Setting | Pinned value |
| --- | --- |
| `core.checkStat` | `default` |
| `core.trustctime` | `true` |
| `core.ignoreStat` | `false` |
| `core.fsmonitor` | `false` |
| `core.untrackedCache` | `false` |
| `core.useReplaceRefs` | `false` |
| `status.showUntrackedFiles` | `all` |
| `status.showStash` | `false` |
| `diff.ignoreSubmodules` | `none` |
| `core.excludesFile` | The platform's null device |
| `core.attributesFile` | The platform's null device |

These are invocation overrides, not reasons to refuse a repository whose
configured values differ. The null device is `NUL` on Windows and `/dev/null`
on Unix. Global and system ignore and attribute files remain disabled. A
repository's own `.git/info/exclude`, in the root or a child, can still hide an
untracked file and cannot be pinned away; this is a limit of the judgment,
not a refusal condition. Repository-local `.git/info/attributes` also remains
active and can affect line-ending judgment.

With those pins, the root and each child use this status command:

```text
git status --porcelain=v2 -z --no-renames --untracked-files=all --ignore-submodules=none
```

The status result must be empty in the root and in every child. There is no
exception for child dirtiness, a root tracked change, a staged pin, or untracked
non-ignored output. Never discard submodule status.

Every gitlink must be initialized with valid associated Git metadata and HEAD
equal to its recorded pin. The index pin must agree with the root commit's tree;
conflicted, duplicate, malformed or unsupported gitlinks refuse. Each child's
own status must be empty. Untracked, non-ignored files reported by Git make
the root or child ineligible. Git-ignored files are outside source identity and
outside the cleanliness judgment, in the root and children alike; tracked
changes still refuse. Nested gitlinks refuse; recursive dependency admission
is outside v1.

Generated output inside a child must be Git-ignored by the repository that
holds it or by a `.gitignore` the generating tool writes into its own output
folder. That `.gitignore` must itself be committed or covered by an ignore rule
(a `*` pattern covers it), so status remains empty. There is no exception list.

In the root and every child, refuse skip-worktree and assume-unchanged index
flags, sparse or partial checkout, and clean or smudge filters including LFS.
The status settings above are pinned rather than refused. Do not refresh an
index to manufacture cleanliness. Disable replacement objects for every Git
operation in the root and children, including status calls made by Git inside
a submodule. Pin `core.useReplaceRefs=false` for those invocations; conformance
must establish that replacement remains disabled through child-status calls.
Unavailable or unsupported evidence is a named ineligibility reason, not a
clean result.

Record and compare the root commit, its tree and every child pin before and
after the run. Root HEAD must be unchanged; every child must still be
initialized at the same pin with unchanged HEAD and eligible status. Recheck
all admission conditions. An uninitialized,
mismatched, dirty or nested child makes the run ineligible with a named reason.
Capture never initializes, fetches or repairs a child to make it eligible.

### Meaning and limits of the evidence

Commit-input v1 credit means the exact Git commit and tree, judged clean by Git
on this checkout. It does not mean a byte-identical or case-faithful checkout.
A consumer whose build depends on case-sensitive paths or exact bytes must not
treat this evidence as proof of those properties. There is no additional
case-folding, Unicode-normalization or filesystem-object identity algorithm
in this committed-input lane. The separate content-v1 admission change
does not turn Git cleanliness into proof of physical spelling.

Git's stat-based verdict can miss a same-size, same-mtime edit, even with the
settings above pinned. Ignored files can influence a run unseen, for example an
ignored lockfile. Source identity therefore does not identify the dependency
environment. Repository-local `.git/info/exclude` can also hide untracked input
from this judgment, and `.git/info/attributes` can affect line-ending judgment;
the same limitations apply to the root and children.

Equal Git identity alone does not prove that children were materialized.
Initialization, matching pins and cleanliness are explicit admission evidence,
not fields mixed into the identity. Deinitializing a child can leave the root
commit id unchanged while making the run ineligible. Do not infer eligibility
from string equality or turn a failed eligibility check into a mismatch value.

## Canonical identities and bounds

For the content-v1 basis, the existing framing remains normative.
All lengths are byte lengths encoded as unsigned 64-bit big-endian integers.
Labels and specified strings are UTF-8; `||` denotes byte concatenation:

```text
C(label, bytes) = u64be(len(label)) || u64be(len(bytes)) || label || bytes
stream = C("schema", "content-v1") || ordinary_entries_in_path_byte_order
ordinary entry = C("entry:" + path + ":" + mode, existing_content_v1_bytes)
source_id = "content-v1:" + lowercase_hex(SHA256(stream))
```

Ordinary path membership, actual mode, ordering and content normalization are
unchanged. There is no new global object-format tag, gitlink payload or
initialized/uninitialized state marker in this stream.

For eligible committed input, with or without gitlinks, use
`git-commit:<full commit id>` and record its tree as local admission evidence.
The existing typed wire form stays `Commit { commit }`; it gains no tree field
or other extension. Do not wrap the commit in a new SHA256 or label it
content-v1. Pins are already in the committed tree;
child file paths, physical object IDs and materialization states are not new
digest entries. Scheme equality is necessary: a content-v1 value never equals
a git-commit value merely because their inputs appear similar.

Neither scheme incorporates the absolute checkout root, host name, PID,
timestamp, run id or store identity. Supported copies of the same commit have
the same git-commit identity, but eligibility must be established independently
on each checkout. Ordinary content-v1 retains its existing platform mode and
line-ending semantics; path independence promises no universal cross-platform
byte equality.

The capture retains one shared deadline and bounded output/path reads. Current
reference limits are 40 seconds, 128 MiB per bounded Git output/file read,
512 MiB total ordinary file bytes and 200,000 listed ordinary paths. Submodule
inspection is bounded as well, with at most 64 gitlinks, 200,000 listed child
status entries and the same overall deadline. Exceeding any bound refuses the whole
basis. Bounds affect availability, not identity values; never accept a
truncated status or pin set. Recheck the commit, tree, pins and child eligibility
before returning success.

## Consumer migration and authority

`ORDINARY_COMMIT_INPUT_DEFAULT` is false today; content-v1 remains the default
basis for named roots, places and execution. `docs/test.md` (Ordinary
committed-input runs) names candidate-commit availability as its condition for
the default switch. This contract's own plan is to make the commit flow the
default once gitlink repositories are admitted. Whether a future consumer may
explicitly select content-v1 for a dirty checkout remains open and must be
decided before that default change; capture and verification themselves never
fall back between bases. Bases compare by exact string equality, so switching
bases causes remeasurement, never a false match.

| Consumer | Source identity use | Additional facts retained |
| --- | --- | --- |
| Test credit, first | Content-v1 hash compatibility and relaxed spelling admission; the general typed git-commit lane uses commit-input v1 eligibility at both boundaries | Recorded tree, child pins/materialization/status, command/check identity, real execution/result, root/work/run/claim/fence and frozen input |
| TermAl review_freeze/review_identity, next | Reuse the appropriate scheme and admission result after explicit versioned consumer migration | Existing review scope, expected independent fingerprint, reviewer identity and any packaging components required by that consumer |
| Engram freeze and launcher, later | Call the same capture contract, through a packaging route still to be chosen | HEAD, index, untracked/cleanliness components and before/after execution identity |

Engram's freeze answers whether the worktree changed, including packaging
state. Source-content equality does not answer that question alone. No consumer
may silently replace its old freeze hash with source_id, reinterpret a stored
fingerprint, or drop a cleanliness or authority check because source_id matches.
TermAl's existing schema-1 review-freeze verifier remains unchanged by the
test-credit rollout and continues its current refusals until its own migration.

The direct source-basis producer and registered committed-input flow must use
the same eligibility rules for the committed-input lane. Reuse the existing typed Commit
record and cleanliness validator where applicable; align Node input capture,
launcher eligibility and Rust input coherence. Removing one producer refusal
does not establish end-to-end credit. This contract does not widen the staged
lane or replace the existing verification wire form.

Engram currently stores source revisions and acceptance fingerprints as opaque
scheme-prefixed strings, as reported by its coordinator. New admitted gitlink
values require no relabelling or bulk store migration. Consumers nevertheless
must support this contract before relying on the new domain; a raw opaque value
does not prove the producer enforced these requirements.

Stored bases are opaque strings. No source-identity change, including gitlink
admission in the commit lane, the content-v1 spelling-admission change or a future
default change, rewrites, recomputes, relabels, invalidates, deletes or re-runs
a stored evaluation, verification or seal. Existing records keep their prior
interpretation and matching. A later default change that would
leave an open item's existing evaluation unmatched must be brought to Greg
with the facts and options before the change; it never happens automatically.

Historical records retain their exact scheme and bytes. In-flight runs retain
their actual producer and frozen input; never recapture and overwrite their
starting evidence to manufacture freshness. When an unsupported consumer or
mixed producer interval prevents a valid comparison, report unavailable
evidence and use that consumer's normal recovery path. Do not infer continuity.
A future incompatible encoding has an explicit new scheme; renaming an old
value cannot make it fresh under that scheme.

This document grants no commit, push, landing, install, restart, cross-root or
verification authority. It changes no repository authority instructions. Any
later authority-text change follows the existing exact-wording process.

## Required conformance evidence

Implementation review must cover these cases before claiming the contract:

For the test-credit consumer, cases 1–5 may be satisfied by existing
content_revision tests for preserved ordinary behavior together with the
spelling-admission and committed-input tests; the implementation's review maps
each case to its test, and an uncovered case is either added or recorded as a follow-up item before the claim,
never silently assumed. A follow-up records missing evidence; it does not certify
the uncovered behavior as tested.

1. Explicitly request `content-v1`. Compute old and new captures at test time
   on no-gitlink fixtures and require
   exact value equality for previously admitted input, except the spelling-only
   collision, which now refuses. The sole admission change allows on-disk
   directory spelling to differ from Git and refuses that collision. Exercise
   that case, distinct tracked case/NFC/NFD paths resolving to one file with
   both paths named on refusal, unchanged distinct-name hard links and terminal
   symlinks, and redirecting-parent refusal. Do not use only a fixed digest
   literal. Include binary data,
   staging/commit without content changes, untracked inclusion, ignored-file
   membership, deletions, file/directory/link changes, modes and line endings.
   Include sparse, conflicted and instrumented race cases to detect accidental
   tightening of the old no-gitlink path.
2. Explicitly request `commit-input v1`. Verify independent eligible checkouts,
   with and without gitlinks, of the
   same commit have the same git-commit identity and recorded tree. Different commits with equal
   trees must differ. Ordinary fixtures retain raw path ordering, Unicode
   names without normalization, mode and line-ending semantics. Confirm that
   repository core.ignoreCase/core.fileMode are retained for commit-input v1,
   while every status setting listed above is pinned on both root and child
   calls and global/system ignore and attribute files are disabled. Preserve
   the stated limits of repository-local `.git/info/exclude` and
   `.git/info/attributes`.
   On a clean no-gitlink fixture eligible for both bases, assert that the
   requested basis determines the returned scheme and that verification derives
   the same basis from the expected scheme. Refusal in either basis must never
   trigger a capture in the other basis.
   Cover index-only gitlink additions, removals and pin changes: commit-input v1
   is ineligible without fallback, while explicitly requested content-v1 retains
   its existing index-based behavior. The latter gains no HEAD-tree refusal.
3. Exercise initialized-at-pin children, wrong HEAD, absent children, nested
   gitlinks, dirty root/child index or worktree, staged pins, malformed ids and
   object-format mismatches and named SHA-256-superproject refusal. A missing
   child may leave the commit id equal but
   must make the run ineligible. Exercise skip-worktree, assume-unchanged,
   sparse/partial state, filters including LFS, and disabled replacement objects
   in both root and child. A replacement tree in the root or in a child must
   not change the committed-input judgment; exercise both direct child status
   and child status invoked by root Git to prove the control reaches each call.
   Verify that differing configured status settings
   receive the specified invocation pins rather than an ineligibility refusal.
   Assert named ineligibility reasons for refused states, not invented identity
   values or credit. Document Git's actual case/byte judgment without claiming
   physical spelling or byte fidelity.
4. Exercise generated output inside a child: Git-ignored files remain outside
   the cleanliness judgment in both root and child, while untracked non-ignored
   output makes the input ineligible. Cover repository ignore rules and a
   generating tool's `.gitignore` inside its own output folder. Tracked changes
   still refuse. Include an ignored lockfile to show that dependency-environment
   identity is not proved.
5. Compare root commit, tree and every child pin at both run boundaries, with
   unchanged root/child HEAD and eligibility at each. Race HEAD, index, files
   and child materialization; inject unavailable reads,
   cancellation and budget exhaustion. Verification errors never become match
   or mismatch. Prove capture leaves source, index, objects and configuration
   unchanged and attempts no network, hook or filter execution.
6. Compare every later consumer's capture against the reference on the same
   fixtures and demonstrate that its distinct packaging, execution and authority
   checks remain enforced. Preserve historical and in-flight record bytes.

### Committed-input case-collision observations

Include these native Windows Git-clone cases with the repository settings
`core.ignoreCase=true` and `core.fileMode=false`. The expected status
observations below were reproduced with those settings. The ellipsis omits
other porcelain-v2 fields; it is not literal Git output.

| Fixture | Expected Git status observation | Committed-input outcome |
| --- | --- | --- |
| Tracked `A.txt` and `a.txt` with different committed blobs, resolving to one file | `1 .M N... … A.txt` | Dirty and ineligible. |
| Tracked `A.txt` and `a.txt` with identical committed blobs, resolving to one file | Empty status | Eligible when the other guards and both-boundary checks pass: the on-disk content equals both committed blobs, within the stated commit/tree scope. |
| Superproject whose initialized child `dep` is at its pin and contains the different-blob fixture | Root status `1 .M S.M. 160000 … dep`; child reports the different-blob modification above | Dirty and ineligible at both the child and root. |

The committed-input lane adds no filesystem case or collision check. It uses
Git's cleanliness result and retains every guard and before/after check above.
The identical-blob outcome does not certify that both distinct paths were
materialized, nor does it change the separate content-v1 collision rule.

Before claiming readiness for PhoenixCodeNav or Questly, validate a successful
capture with the implemented producer on that consumer's actual checkout.
Record the producer revision, admitted component states and any refusal.
Perform this consumer validation after the required implementation installation;
it does not assert that the documentation candidate or the current producer already passes,
and it is not a new prerequisite for landing the implementation.

The implementation must replace the draft content-v1 gitlink payload with the
typed Git identity and end-to-end eligibility rules before it conforms. A clean
status summary or equal commit string alone is insufficient. These are
acceptance requirements, not claims that the docs-only draft ran or passed
those tests.

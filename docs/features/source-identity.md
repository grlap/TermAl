# Source identity v1

Status: current measurements use the complete Engram-owned `source_id`
object. TermAl's former content-revision producers were removed in
`1d63b37a`; stored `content-v1:<digest>` revisions remain readable as
historical records, not an option for new capture.

The object identifies the root commit and tree, recursive submodules and
local changes. The registered committed-input lane adds its existing
eligibility checks for Git repositories with or without gitlinks. A complete
source observation and committed-input eligibility are separate facts; neither
replaces execution, claim or landing evidence.

TermAl owns this canonical document together with Engram. Engram links to it
rather than maintaining a second copy. No source identity certifies the
on-disk case spelling of paths: committed-input eligibility uses Git's
cleanliness verdict, including the equal-blob case below.

## Purpose and rollout

Engram owns source measurement. TermAl's ordinary observations, named-root
capture and Node launcher consume the configured producer's complete
`source_id`; they do not maintain a second content-hash algorithm. The host
checks the producer's executable custody, root and current authority around
capture. An unavailable producer supplies no source basis, not a locally
reconstructed revision.

Other consumers, including TermAl's `review_freeze` and `review_identity`
and Engram's separate freeze tooling, retain their own packaging and authority
checks. Migrating a consumer remains explicit; this documentation correction
does not replace its identity or claim new implementation coverage.

A source identity is not a run, record, review, evaluation or build identity. It
does not prove that a command ran, that it passed, that a root is authorized, or
that a claim or fence is current. Those facts retain their own evidence.

## Capture and verification

The names below describe the logical API, not a newly shipped CLI or wire format:

```text
capture_source(explicit_local_worktree_root)
    -> Captured { source_id, admission_evidence, local_capture_provenance }
     | Error { reason, optional_relative_path }

verify_source(explicit_local_worktree_root, expected_source_id)
    -> Match { captured_source_id }
     | Mismatch { captured_source_id }
     | Error { reason, optional_relative_path }
```

New capture obtains the complete owner object described under
[Canonical native source_id and credit](#canonical-native-source_id-and-credit).
Verification validates the expected object and compares it with a complete
capture from the same owner contract. Only a successful supported capture can
yield match or mismatch. Unavailable files, unsupported states, cancellation,
races, deadlines and failed reads yield errors. An error is never an empty
identity, an invented mismatch, or permission to reuse an earlier match.
Reading a stored historical revision does not request a new capture in its
retired scheme.

For committed input, the existing run record carries the root commit and the
before/after cleanliness judgment. Child initialization, pins and guards are
checked during capture; they do not add a version, child table or wire field.
Failure to establish eligibility names a reason and earns no credit. Knowing
a Git commit id alone is not a successful eligible capture.

The API needs an explicit local root, a trusted installed producer, and an
expected source object for verification. Capture itself needs no project store,
configured home, MCP session, network or write permission.
Read-only reviewers must not execute a helper supplied by the reviewed worktree.
The root is canonicalized and verified as the Git worktree root. Its absolute
location belongs only to local provenance, never to the source identity.

Capture does not fetch, initialize or repair a submodule; run hooks, filters or
external diff programs; write objects or an index; stage files; or modify source
or configuration. Pin the Git executable outside the inspected repository and
retain the existing bounded Git runner, disabled optional locks, disabled
fsmonitor, disabled global/system ignore and attribute files, disabled hooks,
filter refusal, and denied transports/lazy fetching. The committed-input extension reuses each caller's existing Git runner and
isolation. It adds no per-child settings table or status parser and makes no
new claim about replacement-object behavior or byte-faithful checkouts.

At both committed-input run boundaries, repeat the admission predicate below
on the root and its recursive submodules. The root commit must remain unchanged
across the run; its tree identifies the pins. A new root commit is different
input even when its tree is equal. Child dirt, a wrong pin or deinitialization
loses committed-input eligibility. Observed drift during capture refuses its
result.

Equal boundary observations do not prove an atomic filesystem snapshot or
exclude an undetected change-and-revert between observations. Consumers still
need their existing before/after execution and mutation rules.

## Historical content-v1 records

Stored `content-v1:<digest>` values keep their original bytes, attribution and
matching semantics. They remain readable; they are never recomputed, relabelled
as a current `source_id` object, or reconstructed from a new checkout.

The historical encoding reference is `src/content_revision.rs` at commit
`cd79424e8b2d1fa453ab43abb23761152a0d8913`, using `review_freeze_chunk`
and the checked reader in `src/review_freeze.rs`. The producers
`src/content_revision.rs` and `src/content_revision_collisions.rs` were
subsequently deleted in `1d63b37a`. Their path membership, mode, line-ending
and collision rules describe past captures only; they are not a live
admission algorithm or a fallback for unavailable owner measurements.

Retained legacy readers preserve saved revisions and records without
manufacturing new ones. Historical content equality never establishes current
source identity, cleanliness, execution or acceptance freshness.

## General committed input: existing git-commit lane

The registered committed-input test-credit lane admits supported Git
repositories with or without direct gitlinks. It retains the existing typed
`Commit { commit }` input. Its source observations are the complete
Engram-owned objects below, not a separate whole-tree digest.

The root commit identifies its tree and recorded child pins. Two commits with
equal trees are different inputs. Dirty or staged input earns no committed
credit; an explicit committed-input refusal never falls back to a historical
revision. Automatic launcher selection may instead choose its existing plain
run before registration, which earns no committed verification credit.

Engram's registered Commit lane remains SHA-1 only: a full 40-digit lowercase
object id. A SHA-256 root is refused with a named unsupported-format reason,
never truncated or rehashed. This does not widen that wire domain.

### Eligibility at both boundaries

For an automatically observed declared native test, the opening boundary is
the host's completed pre-prompt snapshot, not a capture started after the
command notification. Only the originating turn's first tool activity can
use it. The closing boundary repeats the full `source_id` capture and must
find the same source state, including local changes. Both snapshots and the committed declaration and
artifact pre-image belong to the host; a launcher's claimed input never
substitutes for them. This supports initialized pinned submodules without
reviving a historical content-revision producer.

### Canonical native source_id and credit

The shared source contract is quoted below. Its rollout wording records the
historical transition; it does not mean that a legacy producer remains active.
See also the [native verification contract](execution-observability.md#canonical-source-id-and-eo-credit).

> source_id = ONE object describing the source state, produced only by `engram source` (Engram owns it):
> - root: commit (HEAD) and tree;
> - submodules: path and actual HEAD for each, from `git submodule status --recursive`; expected pins come from the root commit;
> - changes[]: only the local delta, from `git status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none`, with path, status, origin (for renames), mode or type, and the Git blob id from `git hash-object --stdin-paths` (default filters, no -w), computed over the dirty paths only. Deletions are tombstones. Untracked non-ignored files are included. Ignored files are excluded absolutely. Only dirty or off-pin submodules recurse; an uninitialized or conflicted submodule makes the result unavailable. Clean means changes = [].
> Bounds (Greg accepted: „mysle, ze to rozsadne ograniczenie w wiekszosci przypadkow, git diff jest pusty”): at most 1 MiB of JSON and 8 s; overflow, timeout, drift seen by a stat-and-status recheck, filters, or any error means unavailable. A partial result is never returned.
> Never: watcher events, hashing anything in the .git directory, or a full repository or index listing.
> EO credit (Greg: „to jest wystarczajacy warunek ... mysle, ze i tak jest za silny, ale zacznijmy od tego”): the full source_id at S0 (before the prompt is dispatched) equals the full source_id after the declared test; local changes are allowed. The declared test must be the turn's first tool action; the result goes out as the existing verification_record_v1, with the existing artifact, identity and claim checks. There is no clean-repository requirement and no watcher. Clean is only a landing policy.
> Accepted, documented limits: a change fully reverted between S0 and the after capture; anything git status does not report (hidden index flags, edits inside .git); changes to ignored files.
> Rollout: `engram source` gains the new fields in its existing JSON (no v2); TermAl's and Engram's strict readers are updated in the same window. Consumers then move off content-v1 one at a time. Old records stay readable as stored.
> Rule: no new conditions, mechanisms or gates without Greg's explicit word.

The quoted 8 s bound is historical. On 2026-11-07 Greg changed the source
capture bound to 30 s (Asistant #734): „8 sekund jest za mało. Podnieśc do
30 sec. Takie sztywne limity bardzo duzo nas kosztuja poprawek i marnowanego
czasu.” On 2026-11-11 he raised the operation minimum to one minute:
„Min 1 minuta.” Calls still running after ten seconds log a warning and continue.
The 1 MiB output bound is unchanged. TermAl's source-owner operations
retain one admitted phase plan through queueing, authority locks and nested helpers.
The `--version` process has its own 62 s window (60 s plus 2 s process
delivery), with one warning after 10 s that does not abort or resend it.
The subsequent source process retains its admitted window of at least 62 s,
including any longer caller deadline. The aggregate metadata reserve is
124 s, with 5 s HTTP delivery (129 s).
TermAl omits `--timeout-ms` from source-owner arguments.
Before the Engram sync, the source owner's own default is 30 s; TermAl no longer
imposes a source limit and waits at least 60 s plus overhead. This does not claim
an end-to-end 60 s owner limit before that sync.
S0 opening also keeps a 60 s authority reserve for surrounding stages.
The source minimum derives from `scripts/source-id-budget-ms.json`; the shared
operation minimum and process delivery derive from `scripts/engram-call-budget-ms.json`.
Pinned registration reserves the metadata plan before its
60 s claim/durability allowance. The client also funds one full admitted
held-read overrun and 3 s of response delivery: 123 s ordinary, 247 s pinned.
The host's minimum operation window and the client's two claim windows use
`scripts/engram-call-budget-ms.json`, independently of the source budget.
Short reclamation
budgets are raised too; they cannot cut an admitted Engram call short.
Omitting the override works with both the old 30 s and new 60 s owner default;
activation still requires the coordinators' compatibility and execution checks.

Native `registered_at` and check start name the persisted first
host-observed start, not the OS spawn time; any provider timestamp is kept
separately. See [the native verification contract](execution-observability.md#declared-native-source-verification).

Use the existing trusted Git invocation and its unchanged budget. Retain each
repository's own `core.ignoreCase` and `core.fileMode`; do not force a different
filesystem-spelling judgment. The launcher and host retain their existing
environment isolation, disabled fsmonitor, and null-device excludes/attributes
overrides. The compiled host keeps its pinned external Git runner and existing
transport, hook and deadline restrictions. There is no new per-child settings
table and no repository or global configuration write.

1. Require the root top level to equal the requested root, read its full
   SHA-1 HEAD, and refuse configured root clean/smudge/process filters.
2. Run `git submodule status --recursive`. Every reported row must start
   with a space; `-`, `+`, `U` or a command failure refuses. Git owns pin,
   initialization and mapping judgment. Clean nested pinned children are
   allowed; there is no direct-child row-count comparison or separate pin table.
3. For each child path from that output, refuse configured clean/smudge/process
   filters before root status can run them. No root or child file/index list
   is derived, passed or buffered; admission makes no `ls-files` call.
4. Require this existing root status command to report no entries:

   ```text
   git -c status.showUntrackedFiles=all -c diff.ignoreSubmodules=none status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none
   ```

   This is the only dirt check. Git's recursive root status reports tracked
   and untracked child changes; child `ignore` settings cannot hide them.
   The inherited `status.showUntrackedFiles=all` override prevents a child's
   local configuration from hiding untracked input. The inherited
   `diff.ignoreSubmodules=none` override also keeps nested dirt visible.
   Do not add a child porcelain parser or discard a child-dirty root entry.
5. Reread root HEAD and require it unchanged across the capture. The launcher
   uses the complete owner source_id at its admitted, before and after
   boundaries, with no automatic fingerprint. An unavailable measurement
   supplies no partial value and loses eligibility. Stage states and command
   exits remain recorded even when the input comparison refuses the run.

Tracked root or child changes, staged pins and untracked non-ignored output
are ineligible when Git reports them. An edited assume-unchanged or skip-worktree
file can be invisible to status and counts as clean. One root unit test
demonstrates the accepted Git-status limitation, which also applies to children. This is Git cleanliness,
not an assertion that every worktree byte was examined.

Git-ignored files are outside identity and cleanliness judgment in the root and
children alike. Generated child output must be ignored by its repository or by
a `.gitignore` the generating tool writes in its own output folder. That ignore
file must itself be committed or covered by an ignore rule (a `*` covers it).
There is no exception list.

The capture makes a small, bounded number of metadata calls per child and
reuses the existing root status parser. Git also launches internal child
commands: report those actual costs separately from outer call counts. Do not
claim the process count is independent of the number of children. No budget or
timeout increases, child initialization, fetching, repair or index refresh are
part of capture. A failed or unavailable check names a reason, never a clean
result or invented identity.

### Meaning and limits of the evidence

Committed-input credit means the exact Git commit and tree, judged clean by Git
on this checkout. It does not mean a byte-identical or case-faithful checkout.
A consumer whose build depends on case-sensitive paths or exact bytes must not
treat this evidence as proof of those properties. There is no additional
case-folding, Unicode-normalization or filesystem-object identity algorithm
in this committed-input lane. Historical content-hash admission rules do not
turn Git cleanliness into proof of physical spelling.

Git's stat-based verdict can miss a same-size, same-mtime edit, even with the
existing settings. Ignored files can influence a run unseen, for example an
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

For new measurements, the complete owner object is the identity:

```text
source_id = { commit, tree, submodules, changes }
```

The producer's envelope also retains its `git-commit:<full commit id>`
projection, and the registered input stays `Commit { commit }`. Neither is a
substitute for the complete object. Root commits identify their trees and
recorded child pins; actual child commits and local deltas remain in the owner
observation. Do not wrap that object in a new digest or reinterpret a stored
historical revision as one.

The source object incorporates no absolute checkout root, host name, PID,
timestamp, run id or store identity. Supported copies of the same source state
can have equal identities, but eligibility is established independently on each
checkout. Equality promises no universal cross-platform byte or case fidelity.

Current owner capture uses the output and phase bounds described above.
Historical whole-file byte counts and path-list limits belonged to the retired
content producer; they do not describe new ordinary measurements. An exceeded
bound or unavailable capture supplies no partial identity. Repeat the same
consumer admission checks at each execution boundary.

## Consumer migration and authority

`source_revision` in `src/ordinary_commit_input.rs` and its historical capture
wrappers remain `#[cfg(test)]`. Their callers pass the committed-input choice
explicitly. The former Rust default selector and its implicit-choice wrappers
have been removed.

Production ordinary observations use `engram_observation_source_capture`
in `engram_observation_source.rs`; named-root capture uses
`engram_naming_source_basis_until` in `engram_naming_source.rs`. Both call
the configured Engram source owner and retain its complete `source_id`.
New observation bases leave the legacy `source_revision` text empty.
A missing or refused owner does not fall back to a local content fingerprint
or clean-commit revision.

The launcher's independent automatic input selection is enabled: clean
admitted checkouts select the existing Commit lane, while dirty or unsupported
checkouts select a plain run without committed verification credit. Explicit
`--input commit` remains strict. This production selection is independent of
the historical Rust helpers.

Automatic launcher input and its host readers use only the existing complete
`request.sourceId`, `results.sourceIdBefore` and `results.sourceIdAfter`.
Equal dirty values qualify for this comparison; any difference, including a
newly untracked file, cannot qualify. Missing or invalid objects never compare
as equal. Waits retain request/locator identity and authority guards without a
parent-held source copy. Detail and remote completion carry actual observations
within existing bounds. Historical fingerprints stay attributed history;
review-freeze identity remains deferred. See [the launcher contract](../test.md#ordinary-committed-input-runs---input-commit).

| Consumer | Source identity use | Additional facts retained |
| --- | --- | --- |
| Ordinary observations and test credit | Complete Engram-owned source_id; the registered Commit lane also checks its eligibility predicate at both boundaries | Root commit, command/check identity, real execution/result, root/work/run/claim/fence and frozen input |
| TermAl review_freeze/review_identity, next | Reuse the appropriate scheme and admission result after explicit versioned consumer migration | Existing review scope, expected independent fingerprint, reviewer identity and any packaging components required by that consumer |
| Engram's separate freeze consumer | Requires explicit migration without replacing its packaging checks | HEAD, index, untracked/cleanliness components and before/after execution identity |

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

Stored historical source revisions and acceptance fingerprints retain their
opaque scheme-prefixed strings and prior interpretation. Current source
observations carry the complete owner object; reading historical strings does
not turn them into new observations or prove the current producer enforced
these requirements.

No source-identity change rewrites, recomputes, relabels, invalidates, deletes
or re-runs a stored evaluation, verification or seal. A later default change
that would leave an open item's existing evaluation unmatched must be brought
to Greg with the facts and options before the change; it never happens
automatically.

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

For the test-credit consumer, use the owner-source and committed-input
evidence for the implemented behavior. Review maps each case to its evidence;
an uncovered case is added or recorded as a follow-up before the claim, never
silently assumed. A follow-up records missing evidence; it does not certify
the uncovered behavior as tested.

1. Preserve stored historical revisions with their exact schemes, bytes and
   matching semantics. New measurements use complete owner observations;
   an unavailable owner never triggers a retired content capture or
   reconstruction of an old fingerprint.
2. Request the existing committed-input lane on independent eligible
   checkouts, with and without gitlinks. Equal commits have equal identities;
   different commits with equal trees differ. Preserve the no-gitlink path's
   existing behavior and repository ignoreCase/fileMode semantics. An explicit
   committed-input refusal never downgrades to a plain run or historical
   revision.
   Check actual outer and internal Git call counts without a timeout increase.
3. Exercise initialized pinned children, tracked and untracked child dirt,
   wrong pins, deinitialization, root dirt, staged pins,
   filters, clean nested pinned links and missing `.gitmodules` mappings with a clean
   child still at its pin. Refusals name their reasons. Cover a named
   SHA-256-superproject refusal, old-form directories and `.git` files, and
   ensure a child's filter is refused before recursive status could execute it.
   Do not claim byte or physical spelling fidelity from Git cleanliness.
4. Exercise generated output inside a child: Git-ignored files remain outside
   the cleanliness judgment in both root and child, while untracked non-ignored
   output makes the input ineligible. Cover repository ignore rules and a
   generating tool's `.gitignore` inside its own output folder. Tracked changes
   still refuse. Include an ignored lockfile to show that dependency-environment
   identity is not proved.
5. Repeat the same root/child predicate before and after execution. A new
   root commit, child dirt, wrong pin or unavailable capture loses eligibility;
   the actual command exit and outcome are preserved. Retain existing
   fingerprint and final-HEAD checks and preserve failed original runs before
   diagnosis. Capture does not initialize, fetch, repair or modify source,
   index or configuration. Verification errors never become a match.
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
materialized.

Before claiming readiness for PhoenixCodeNav or Questly, validate a successful
capture with the implemented producer on that consumer's actual checkout.
Record the producer revision, admitted component states and any refusal.
Perform this consumer validation after the required implementation installation;
it does not assert that the documentation candidate or the current producer already passes,
and it is not a new prerequisite for landing the implementation.

Current implementations use the complete owner identity and the typed Commit
input's end-to-end eligibility rules. The former draft's content-hash gitlink
payload is historical, not a new capture option. A clean status summary or
equal commit string alone is insufficient. These are
acceptance requirements, not claims that a documentation draft ran or passed
those tests.

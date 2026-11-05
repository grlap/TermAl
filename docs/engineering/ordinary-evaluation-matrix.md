# Ordinary work and acceptance evaluation fixture

The ignored live tests under
`tests::engram_host_adapter::root_recovery_live::named_root_completion_live::registered_launcher::evaluation_matrix`
exercise ordinary work against a pinned Engram binary and a disposable store.
Run them with the maintained `live` launcher documented in [test.md](../test.md).
The Linux runner must verify its own absolute binary path and SHA-256; a
Windows pin is not evidence of the Linux executable's identity.

The fixture uses production `AppState`, `app_router`, real HTTP requests,
the candidate's actual launcher scripts, Git commits and actual native test
runner output. The maintained `live` stage passes `--nocapture`, so all
`MATRIX_*` provenance and diagnostic output remains in `engram-live.log` on
passing as well as failing runs. Codex `commandExecution` and Claude Bash
tool-use/result shapes
enter the production command handlers. The additional `claude_frames` module
at the same mount goes through `apply_claude_frame`, with the actual dispatched
host prompt, ownership reservation and echo, runtime token and turn generation,
Write and Bash frames, and the result frame that closes the turn. It asserts
that the foreground Bash entry really exists in the outstanding-work registry
with that prompt's provenance during the check, and retires only on its
correlated result. The parser-only Claude case in `cases` does not establish
that boundary. Claude reports its ordinary one-call
`pushd` form. A separate Codex row reports a plain one-call `pushd` line.
The plain absolute `pushd "DIR" && TEST` prefix is recognized by the
current production parser for Codex too; it is a placed control, not proof of
the historical unplaced shape. A separate row reports the retained
`pwsh.exe -Command 'pushd "DIR" && TEST'` wrapper through Codex ingress,
with no outer cwd invented, and requires the ordinary check to remain
unplaced. The fixture substitutes its executable basename, disposable root
and native test command; it does not execute Windows PowerShell on Linux.
The retained host command history supplied the wrapper text. A later recovered
provider tool-call input also supplied a workdir, with doubled separators; the
raw app-server command notification and its cwd normalization were not retained.
A distinct row runs the real registered
launcher with provider command telemetry absent and asserts that no ordinary
runtime check was injected. It does not claim to reconstruct the historical
outer cwd or full provider frame, which the retained command row cannot establish.

Test builds omit the periodic scan and verification threads. The fixture
advances their production entry points (`refresh_test_runs` and
`engram_verification_records_tick`), then reads the actual receipt back from
SQLite. Placed checks require `Settled`. The wrapped and absent-telemetry rows
instead require the real `Received` receipt and absence of a launch observation;
they keep that state visible while continuing through evaluation and completion.
Receiving a producer receipt is not passing this row: a later refusal fails it.
The fixture never calls the older fixture's manual prepare/send/associate/
settle helper and never writes launcher request/results JSON itself.

Evaluation requests and submissions use the HTTP endpoints behind
`termal_evaluate_acceptance` and `termal_submit_acceptance_evaluation`. The
provider is simulated, but the distinct read-only evaluator delegation, host
brief, actual Engram invocation, immutable evaluation and receipt are real.
Passes vary their citations: a real verification, and a real holder note on
an unbound criterion. Completion uses the ordinary holder's Engram `done`
CLI path, with a *fresh* production committed-source measurement, and reads
the resulting completion seal. No cached declaration is substituted for that
measurement. TermAl has no work-completion HTTP endpoint to substitute here.

The registered committed-input lane decides from the real check and the named
root at each decision: the root must be clean under the launcher's Git rule
and HEAD must equal the checked commit. A different commit is refused even
when its tree has identical bytes. Turn reports remain history; their timing,
absence or source-change classification does not decide this lane. The
latest result for the required check wins: a newer failure on the same
commit blocks an older pass. A dirty or otherwise ineligible run cannot be
promoted to passed evidence by cleaning the root afterward. Typed evidence,
work/run/root authority and independent judgment remain required.

Each row owns its own disposable store and original test result. Both orders
of the closing-report race are explicitly gated at the real source-sighting
RPC, before sending it: receipt before successor dispatch; receipt after
successor dispatch; receipt after successor close. Several rows keep the
report blocked through evaluation and completion, so a settled report cannot
be a hidden prerequisite. They release it as soon as the real holder `done`
command returns, before the independent post-seal history and object reads;
the unchanged deadlock guard does not cover those unrelated audit reads.
Another row lets an
unchanged holder close after the evaluator's evidence basis and before its
submission while its report is still blocked. No sleep or timing comparison
establishes these orders.
Full-frame Claude rows run edit, native Git commit and the registered check
in one turn, both on the first turn and after prior ordinary work that actually
ran a check. No empty setup turn supplies their baseline. Additional Codex
and full-frame Claude rows run check A, edit and commit, then check B without
closing the turn in between. They cite B's real record and retain A's original
record and results unchanged. A further real edit after B stays refused.
The delayed-close composition also holds A's real source-sighting RPC while
B edits, commits and finishes its own check. Delivering A's original sighting
must preserve B's later checked input, and cannot cover a real edit after B.
A three-turn diagnostic also holds A's actual delivery while checked B closes,
then lets C edit and commit before B's source measurement can run on that
worker. It records the actual B/C report attribution and feed plus evaluation
and ordinary completion refusals, preserving B's real result and timestamps.
It does not prescribe the producer's intermediate attribution.

A failed native check on clean input can produce a settled verification
record whose result is **failed**. That is an execution fact, not passed
credit. The fail/fix/recheck row retains that original failure and completes
using only the later passing input; the fixture never equates a settled
receipt with a passing result.

The evaluator-phase row requires evaluator creation before an edit and then
submission refusal. The `decisions` module records a successful evaluation,
edit or commit without another turn report, and call ordinary `done` with the
old declared revision. The actual current root must decide; that declaration
cannot hide the edit. Same-commit pass/fail rows use an ignored native-test
switch to produce two real outcomes on the same tracked code, and test early
selection and completion separately. The no-report dirty/new-commit controls
try both another evaluation and ordinary completion after a valid evaluation.
The old evaluation and verification
objects remain unchanged. An unconfirmed-opening pair ports the earlier
host regression without requiring a completion endpoint or upgrading the
captured opening. A handoff away and back reads the canonical root again and
requires the same generation to accept its original check, without another
naming or check. A separate release/recover row requires refusal while the
root is unbound, then names the same root at a new generation, refuses the old
check there, and accepts a fresh check on the unchanged commit. These are real
CLI transitions. After a refused evaluator submission, the simulated provider
reports the refusal and completes its turn through the runtime lifecycle;
the fixture checks that the delegation ended without recording an evaluation
before requesting a fresh judgment. No active-evaluator guard is bypassed.
There is no stored claim edit and no clock advance. Expiry
recovery itself remains covered by Engram's core tests, not by a host timing
assumption in this fixture. Any terminal old-scope closing diagnostics remain
intact across evaluation and completion.

The separate `inflight` module keeps a real native test in its body at a
loopback TCP phase gate while the holder closes and the real producer answers
that closing RPC. Only then may the native test finish. Its original
completion reaches production indexing and verification afterwards. This
keeps the actual timestamps and feed positions; an assumed first-close
change does not by itself mean that a later check must be refused. The TCP
address lives in ignored fixture output, just like other test coordination
state, and never determines a fabricated result or producer record.

Positive rows cover first close, bound and unbound criteria, absent command telemetry,
Claude placement, recovered claim and renamed root, ordinary mail wake,
unchanged later turns, edit/commit/check, and fail/fix/commit/check in one turn.
Negative rows preserve real changes in the checking turn, a later turn and
during evaluation; delayed delivery of the original completed result;
dirty pre-run input; a different clean commit; and asserted prose that cannot
borrow Git provenance for a bound pass. The dirty row asserts the actual
launcher refusal and absence of a passed verification; it does not pretend
there is a passed record to cite.

An edit after the check that is reverted to the checked commit before a
decision is accepted, whether or not the intermediate edit was measured.
Equal endpoints establish that the final input was checked; they establish
nothing about the interval between them. Both ordinary-turn and root-only
measured-change rows preserve the original observation objects after the
revert and completion. This does not claim that Git endpoints prove which
bytes a test consumed throughout its execution interval.

The `engram_launcher` row uses a disposable clone of Engram and its own
unmodified maintained scripts, rather than TermAl's launcher copied into a
minimal fixture. Set `TERMAL_TEST_ENGRAM_LAUNCHER_ROOT` to an absolute source
repository and `TERMAL_TEST_ENGRAM_LAUNCHER_COMMIT` to its exact 40-hex commit.
The row checks out that commit in the clone, records its identity, and commits
only its disposable project binding and small native test before ordinary
edit/commit/check work. Its real command is
`node scripts/test-launcher.mjs focused --input commit -- node --test matrix-engram-check.test.mjs`
with the fixture's actual registration context. The executable pin and launcher
source commit are separate inputs and must both be recorded. It runs no
production build and changes nothing in the source Engram repository. This
row is required on both Linux and Windows for the cross-project proof.
The foreground `STARTED` receipt is accepted only with `host-process-wait`
and the actual terminal artifacts; a detached startup receipt is insufficient.

The `policy_off` rows change only their disposable store's acceptance policy.
They keep the criterion bound to a real registered test, disable required
evaluations through the normal CLI, read the policy back and call ordinary
`done` without recording an evaluation. Clean checked input and an edit
reverted before the decision seal. A dirty root, a different clean commit
with identical tree bytes, and an older check followed by a committed source
edit refuse without a seal. The rows verify that no evaluation object was
created and that all original objects remain unchanged. Evaluation refusals
are tested with evaluations on: an `EvaluationDisabled` refusal would say
nothing about Git currency. No direct unbound citation route is invented for
`done`, whose normal CLI accepts no such evidence argument.

The ten original clean-check false-refusal rows are the agreed superset for
the producer's four traced no-edit cases; the producer could not identify
four exact historical locators. Their original results remain historical
evidence rather than a claim that all ten had the same incident provenance.

The fixture does **not** cover real provider pacing or a real host process's
startup. Those remain part of the next authorized live window. A full
`AppState` reload row is not included yet: the existing fixture owns active
provider channels, control-process cleanup and worker lifetimes, so replacing
the state safely requires a restart fixture rather than copying a session
row. No row-roundtrip is presented as restart evidence.

Every new row must first run against the unchanged product input. Preserve
and report its exact refusal (or explain why that row already passes); do not
change the row to force the historical diagnosis. Passing fixture results
will not establish that ordinary live work has passed, and the next real
window can still expose pacing the fixture does not reproduce. No existing
evaluation, verification or seal is rewritten or rerun by these tests.

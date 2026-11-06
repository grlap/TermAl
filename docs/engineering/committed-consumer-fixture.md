# Disposable committed-input consumer fixture

The ignored tests in `engram_committed_consumers_live.rs` reuse the compiled
evaluation matrix's real AppState/router, registration, verification delivery,
independent evaluation endpoint and holder CLI completion path. Only provider
ingress is simulated. They use private Engram homes and fresh Git clones; they
never write to a live consumer checkout or live store.

The fixture first clones the actual committed objects and initializes direct
children from explicitly supplied object donors. Donors are read only and must
remain retained until all rows finish. The tracked consumer files and original
root commit are preserved. A tracked `.engram-project` is kept; an absent one
gets a disposable binding ignored through the clone's own Git metadata. Setup
finishes before the disposable authority, work, claim and named root are made.
Capture itself performs no initialization, fetching or repair.

The actual launcher library's existing `createRun({root, automaticInput:true})`
and `executeRun` entry points run a short maintained Node check from the real
consumer root. No launcher files are installed or committed there. Its actual
driver invocation is reported through the normal provider event handlers. This
invocation is outside the ordinary CLI recognizer: no ordinary Test/Build
observation or durable association is invented. Production refresh and
verification delivery must return a genuine Engram `Received` receipt. The
independent evaluator cites its real verification records, and ordinary `done`
must produce a real stored seal.

Each negative starts with an actual eligible check and recorded independent
evaluation, then changes only its own disposable clone. Re-evaluation and
completion with the old declared revision must refuse without a seal. All
earlier canonical object bytes and the original request/results bytes must stay
unchanged. Cases cover child tracked/untracked dirt, local configuration hiding
untracked files, wrong pins, deinitialization, clean/smudge/process filters,
root dirt, a new root commit and missing mappings. A separate positive adds
a tiny initialized nested child and commits its pins in the disposable clone.
Assume-unchanged/skip-worktree edits are documented Git-status limitations,
not joint negative rows; one ordinary-reader unit test demonstrates the limitation. Each case has its own libtest name and private
fixture; a failure does not substitute for an unrun row.

The root/pin constants are the real Questly commit `75250b57407b` and Phoenix
commit `74ae7b25d18b` with their recorded direct child pins. Set
`TERMAL_TEST_SUBMODULE_CONSUMERS` to an absolute JSON manifest path under the
runner's repository, with this shape (replace the example donor paths):

```json
{
  "questly": {
    "root": "/absolute/retained/questly",
    "children": {"rincon-common": "/absolute/retained/rincon-common"}
  },
  "phoenix": {
    "root": "/absolute/retained/PhoenixCodeNav",
    "children": {
      "external/fsharp": "/absolute/retained/fsharp",
      "external/roslyn": "/absolute/retained/roslyn"
    }
  }
}
```

Windows paths are absolute drive paths in JSON. The fixture records the
manifest SHA-256, actual root commit/tree and child pins. Donors may contain
dirty work files: `clone --shared --no-checkout` reads committed objects only.
All resulting writes are confined to the test-owned root and home.

Use the maintained pinned `live` launcher with exact test names from libtest's
listing. Their module prefix is:

```text
tests::engram_host_adapter::root_recovery_live::named_root_completion_live::registered_launcher::evaluation_matrix::committed_consumers::
```

`repository_adapter_smoke_reaches_real_evaluation_and_seal` needs only the
reviewed Engram binary pin and exercises the same adapter on a tiny no-child
checkout. It is readiness evidence, not a real-consumer row. The twenty-six consumer
rows need the reviewed matching reader's exact source/binary pin and the
manifest. The joint plan runs Questly on Linux and Windows, and Phoenix on Windows.
Use the coordinator's exact current-platform selection before claiming completion. Preserve full uncaptured diagnostics and original artifacts.
If the matching Engram candidate changes, repeat the joint rows on the new pin.

These tests do not certify installed-build behavior on live checkouts,
filesystem byte identity beyond Git's verdict, atomic capture, process custody
or capture latency. A passing full gate alone does not execute ignored rows.

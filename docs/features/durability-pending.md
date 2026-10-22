# Durability pending

Related: [Engram host adapter](engram-host-adapter.md),
[test plan and launcher](../test.md).

When a successfully enqueued durability fence expires before its exact write
is acknowledged, these explicit operation boundaries return HTTP 503 with a
versioned JSON error instead of describing the write as rejected:

| Operation | Existing identity |
| --- | --- |
| `test_run_registration` | `runId`, immutable `requestDigest`, `ownerSessionId` |
| `acceptance_evaluation_submission` | `delegationId`, `attemptKey`, `evaluatorSessionId` |
| `named_root_naming` (name or clear) | work/claim, generation if allocated, authority owner/version, original requested work/path/session/action |
| `named_root_release` | exact selection and release audit, or the older authority owner being settled before any release audit exists |

For example, the registration response has this shape:

```json
{
  "error": "the enqueued write may still commit; retry the unchanged original request",
  "contract": "termal-durability-pending",
  "version": 1,
  "code": "durability_pending",
  "outcome": "pending",
  "operation": "test_run_registration",
  "identity": {
    "runId": "test-original-run",
    "requestDigest": "original-immutable-request-digest",
    "ownerSessionId": "original-session"
  },
  "retry": { "mode": "same_request" }
}
```

`pending` is an error outcome, not success and not proof of non-application.
The write may commit after the caller receives it. Retain the complete original
request and identity; an explicit retry must resend that unchanged request.
Do not mint a new run, attempt, generation or release audit to resolve it.
The `retry` field describes the safe request, not an instruction for an
unbounded or automatic retry loop.

The launcher stops before starting any test stage on pending registration.
The delegation MCP bridge validates the status, version, operation, endpoint
and original caller/request context, then returns `isError: true`, with the
same validated envelope in text and `structuredContent`. It strips unknown
extensions and does not automatically re-POST. Pending is distinct from both
transport-unknown errors and safe-replay no-write refusals. Evaluator-start
requests (the plural acceptance-evaluations endpoint) do not acquire the
submission contract. No new CLI command or request argument is introduced.

## Identity, replay and restart

An unchanged in-process retry settles the existing durable operation and must
still satisfy its exact acknowledgement and ownership checks. Registration
retains the immutable request digest; evaluation retains the original attempt
and verdicts. Changed payloads or owners conflict rather than replacing them.
No test stage, recorded-success receipt, naming authority or newly free root
slot is published merely because a fence was queued or expired.

Naming accepts only the existing `work` and optional `path` fields: there is
no new identity argument, header or key. A first-contact guard can expire
before a generation exists; its identity honestly reports `generation: null`
and the exact guard owner. A clear that reached its publication fence retains
the original cleared generation in its identity.

A canonical release keeps its original audit and owner across Prepared,
Candidate and Published recovery. A retained Candidate is settled before a
fresh exact read is prepared under that same owner; an old phase's ACK cannot
authorize that new read. An existing own retirement notice needs only its
remaining publication/confirmation, not another retirement. A changed owner,
binding or request still conflicts. If an older Candidate must settle before
a new release can even be prepared, pending reports `releaseStage:
"prior_authority"`, its exact owner/version and `requestedSelection`, with
`auditId: null`. There is no phantom release audit to retry or restore.

After restart, the loader's durable state and the caller's current authority
decide an unchanged request: current state, conflict, or a fresh application.
An absent runtime/local binding mirror is not proof that an earlier operation
was never applied, and a loaded record is not a fabricated ACK for an old
waiter. A recorded evaluation remains recorded; a stopped evaluator cannot
use that old identity to start a different write. A retained registration's
boot monitoring-interrupted marker is separate from its immutable identity.

## Error boundaries and verification

Only the exact post-enqueue `Deadline` fact activates this contract at the
explicit boundaries above. Write/readback failure, supersession, stopped or
shutdown writer, ordinary unavailability, transport loss and other conflicts
keep their existing error categories; message text containing “Deadline” is
not typed evidence. Shared authority maintenance helpers do not turn into
public pending APIs. Nothing changes persistence scheduling, batching,
SQLite pragmas, admission/retry budgets or deadlines.

The deterministic regressions use disposable SQLite stores, real delta
collection and content-fence readback, and explicit clock/phase boundaries.
They cover late writes, unchanged replay, reload decisions, stale owners or
content and negative error classes without sleeps or synthetic load. Loader
tests construct fresh state from real SQLite; they do not claim to exercise
an operating-system process kill/restart. HTTP bridge fixtures exercise the
actual single-attempt decoder and MCP result path. The maintained gate in
the [test plan](../test.md) remains required for the final input.

# Executor operation diagnostics

`GET /api/federation/executor-operation?centralHostId=HOST&operationKey=KEY`
is a read-only local operator API on the executor host. Both query fields
are required. The central host and operation key select one retained journal
operation; a similarly named operation belonging to another central does not
match. This route is separate from the authenticated federation outcome RPC
and from the paged [journal diagnostics](federation-journal.md).

The `operation` object reports the captured target binding, command kind,
executor stage, effect state and exact `queueEntryId` when one has been
recorded. A queue entry is an association, not proof of runtime acceptance.
In particular, a processed journal row does not turn an uncertain effect
into a confirmed one.

The `recipient` object distinguishes an available current binding from an
unavailable or stale recipient. An available recipient has `pendingPrompts`,
`pendingPromptCount` and `pendingPromptsTruncated`. The array contains the
first 100 queue entries in queue order, with metadata only. An empty array
means the matching recipient queue was empty when sampled. An unavailable
recipient has null queue fields; it does not mean an empty queue. The
operation's recorded queue entry may already have left the pending queue.
`receiptDisposition` and `queuePosition` describe that operation's exact
delivery receipt and current queue position independently of the journal
stage. `queuePaused` reports the explicit dispatch pause; it is not a list
of all reasons a prompt may be waiting. `status` and `stopping` report the
current session state. A journal ownership mirror is marked `mirror: true`
with recipient availability `notLocalExecutor`, and carries no local queue
or receipt attribution.

Prompt text, expanded text, attachments, operation payloads and private
runtime acknowledgements are omitted. The normal session GET response and
the paged journal's evidence-redaction contract are unchanged.

The journal read and live recipient snapshot are sequential observations,
not one atomic snapshot across SQLite and the runtime. Polling does not
provide stage history or prove who caused an event. Reading never enables
federation, initializes storage, resumes a paused queue, retries recovery,
dispatches work, settles a receipt or acknowledges a mailbox message.

Missing or invalid query fields return 400, and an absent exact operation
returns 404. Storage errors return 500 with no private error details.
An unresolved local host identity returns 503; an identity change during the
read returns 409 and requires a new read. Neither is reported as a mirror.
Availability is `available`, `missing`, `staleBinding` or `notLocalExecutor`;
the latter three withhold recipient state and queue fields. Read it before
interpreting queue fields.

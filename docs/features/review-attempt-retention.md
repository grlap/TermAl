# Review attempt results

A reviewer delegation can run more than once. Its submission attempt identifies
one review, while the delegation and child session identify the continuing
relationship. A follow-up must retain the terminal attempt before clearing its
current result or admitting another prompt.

The structured reviewer submission and the final raw prose have separate
coverage. The structured envelope contains findings, inspected files, commands
and notes. It does not contain the final assistant transcript. Missing raw
prose is reported as unavailable; a compact summary or a newer transcript is
never substituted as historical raw output.

## Selecting an attempt

The result APIs and `termal_get_session_result` accept `submissionAttempt`.
Omitting it keeps the current-attempt selection. Retained results return
`submissionAttempt` and `resultId`. An attempt-only lookup resolves its latest
confirmed content version, meaning its greatest validated publication ordinal,
including when it is the current attempt. Capturing the actual current cut
precedes that selection; a capture failure is returned rather than hidden by
an older publication. If the current cut returns from A through confirmed C
back to A, explicit attempt-only reads still select C. Internal follow-up
custody retains and validates actual A before rearm without republishing A or
consuming an extra slot. A semantic repair of that same attempt receives a
new result identity; an older published identity remains readable unchanged.
Pin both when reading output pages, including
continuations using the returned `nextOffsetBytes`. A result identity mismatch,
unavailable attempt or corrupt retained record is an explicit error.

Parent historical reads retain existing parent/delegation authorization and
result write-policy filtering. Other native roots of the original project use
the exact read described below. Neither read recovers an older result into the current
delegation, refresh its lifecycle, complete a current wait or run its child.
Capturing the current attempt first runs existing durable-submission recovery
and terminal refresh; selecting an already pinned historical identity does not.
Retained host observations belong to the original terminal attempt.

```text
GET /api/sessions/PARENT/delegations/DELEGATION/result?submissionAttempt=1
GET /api/sessions/PARENT/delegations/DELEGATION/result/output?submissionAttempt=1&resultId=RESULT&offsetBytes=0&limitBytes=8192
```

Legacy attempts with no retained result or output remain unavailable. This
feature does not reconstruct evidence lost before retention was installed.
A legacy reviewer whose attempt counter is zero receives an explicit conflict
for capture or follow-up: its identity cannot safely be inferred or renumbered.
Its current unpinned evidence remains accessible, and a new delegation provides
the supported continuation.
Host custody is bounded; retention failures refuse the destructive follow-up
and leave its terminal result available. No earlier cited attempt is evicted to
make room for a newer one. Operator exports complement host custody and should
be retained according to the tasks that cite them.

The complete serialized host slot, including its packet, original
submission provenance, raw prose, JSON escaping and authentication tag, must
fit the existing 256 KiB mailbox body limit together with its publication proof.
At most 64 review submission attempts are admitted per delegation, with a shared
limit of 64 slots for all confirmed versions, pending captures and reservations.
A recut consumes a slot. Repairs can therefore exhaust the budget before
attempt 64. An admitted attempt's first capture owns a protected reservation
that another recut cannot consume. A large legal submission can therefore
still exceed the combined snapshot limit. No prose, findings or provenance are
dropped to make it fit; use a new delegation when the attempt capacity is
exhausted. Repeating an identical retention operation consumes no extra slot.
There is at most one replaceable unpublished candidate per delegation.
When a failed recut becomes obsolete, its unpublished payload is cleared by an
authenticated transaction before confirmed-cut reuse or a newer attempt's
capture. Its slot remains charged to its original attempt; this neither refunds
capacity nor consumes the newer attempt's protected reservation.

Candidates use a distinct host-only versioned kind under the existing
review-result mailbox owner. A candidate is unavailable until a separately
authenticated publication proof binds its digest, routing, attempt and confirmed
predecessor. Pending replacement and publication compare the exact prior bytes
and predecessor within the existing store transaction. Confirmed records are
immutable; a failed recheck can replace only the unpublished candidate. Follow-up
admission and final rearm independently check the actual terminal packet and raw
prose being cleared. A purpose-specific per-store MAC key in
coordination metadata authenticates the full snapshot; generic message APIs
cannot sign an arbitrary payload. Missing or corrupt keys over retained
history are errors, not permission to regenerate a key. This protects against
message/API forgery, not an actor who can read the secret or alter the host
database. Keys never enter exported packets or raw pages.

Restoring an archived Codex child can append a host-authored assistant note
before its follow-up prompt. The same caller reservation retains the actual
post-restore cut before admitting that prompt. If the selected raw text changes,
this creates another immutable version within the shared slot budget; it does
not make the host note reviewer-authored output. Earlier result identities and
pages still expose the original prose when pinned. After a restore-driven recut,
an attempt-only lookup selects the latest confirmed version, whose raw output
can be that host note rather than the reviewer's prose. Export before follow-up,
or retain the original `resultId` and pin it to retrieve the earlier prose.
The next attempt's protected capture reservation remains unchanged. Cancellation,
reservation replacement, later drift or a failed recapture refuses admission
through the existing restore-compensation path.

The coordination reader floor is `7-review-custody-index-1`. Boot upgrades the
exact `6-executor-outcomes-1` predecessor transactionally, adding only the
`mailbox_messages_topic_key_sender` covering index and advancing the marker.
Earlier supported floors follow the existing migrations. Index construction
finishes before the mailbox connection mutex and request serving exist; reads
never create an index. Known-child lookups use the sender/key range, and fully
orphaned archive reads use the topic/key covering range. Each lookup selects at
most 65 keys, rejects capacity overflow, and fetches at most 64 message bodies.
No primary schema changes. Older binaries reject the new coordination floor;
do not reset or delete a database to work around a downgrade refusal.

## Immutable producer exports

Use the standard exporter for each attempt before requesting a follow-up:

```text
node scripts/export-review-attempt.mjs --base-url http://127.0.0.1:PORT --parent PARENT --delegation DELEGATION --attempt 1 --output-root .evidence/review-attempts
```

The exporter reads the explicit attempt's packet and every pinned raw-output
page through `complete: true`. It rejects summary fallback, mixed identities,
missing bytes and invalid page boundaries. It publishes
`DELEGATION/attempt-1/RESULT_ID/` containing the original packet, original page responses,
unmodified full prose and a manifest with result identity, byte counts and
SHA-256 hashes. When provenance explicitly reports unavailable raw output, it
publishes the original packet and a manifest marking that gap, without a prose
file or a claim of complete output coverage. Attempt 2 uses a different directory.
Re-exporting the same retained content is an idempotent no-op even when the
response envelope's current revision or server instance changes. The exporter
validates the first export's raw files against its original hashes and preserves
those exact bytes and manifest. Changed retained content or damaged artifacts
are refused and never overwrite its files. An external latest index may reference
these manifests without replacing them.
The `--parent` route argument names the requesting session; an eligible reader
supplies its own ID. New manifests record that observation as `requesterSessionId`.
It is excluded from retained-content identity comparison, as is the legacy
manifest's `parentSessionId`, so another eligible reader can reuse the same
export without changing the first manifest or its provenance.
To export an earlier repaired cut, append `--result-id RESULT_ID`; the packet
lookup and every output page must resolve that exact identity.

Outputs stay inside the repository, with linked ancestor directories refused.
An interrupted staging directory is not a published manifest. Export failures
do not change the delegation or retry its review. Review findings, execution
evidence, full gates and acceptance retain their existing meanings.
## Automatic reads by another root in the original project

An actual native root session associated with the original review project can
call `termal_get_session_result` as itself with the original `delegationId`,
`submissionAttempt` and immutable `resultId`. Use that same selection for every
`outputOffset`/`outputLimit` page. This reads an already confirmed archive
publication; it does not retain a new cut or promote a provisional submission.
The current parent-scoped view and parent attempt-only selection keep their
existing behavior. Other roots must supply the exact result identity.

The host records the original project at commissioning and in the authenticated
archive snapshot, including an explicit recorded-without-project basis. A new
no-project review never gains project authority from later session changes.
A legacy association is accepted only from consistent native
stored parent/child facts. Otherwise the read reports `project_basis_missing`;
working directories, briefs, display names and tracker focus supply no authority.
Children, hidden or removed sessions, proxies and roots in another project are
excluded. No manual grant, parent consent, coordinator title or outage detector
is required. Stop and pause do not prevent this read or grant execution power.

The exact read returns the same original packet and provenance. The additional
`reviewAttemptState` describes the original project, the currently recorded
attempt when available, and whether the selected attempt is `published`,
`superseded`, `published_head_unavailable`, or `published_current_unavailable`.
Superseded includes an earlier publication within the same attempt and an
earlier attempt once the live attempt counter advances, even while its successor
is still pending. A verified later publication also proves supersession when live
parent and delegation metadata are gone. If damaged
later rows prevent proving the head, the selected original stays readable with
`published_head_unavailable`. This live observation is excluded from immutable
export comparisons; existing original bytes and manifest hashes remain verified
and untouched. A historical publication never
certifies a later input. Missing, pending/unpromoted and quarantined publications
return explicit errors instead of a clean result. Validated failed packets retain
their original failed status. Missing raw output has no summary fallback.
The parent's pinned path preserves its existing explicit missing/corrupt
submission provenance in a 200 response. An eligible non-parent instead receives
409 `review_result_unvalidated` for that same unvalidated publication.

Reads are nonexclusive and do not change lifecycle, claims, waits, mailbox
acknowledgements or dispatch metadata. Another eligible reader and a returning
parent may retrieve the same original result concurrently. This grants no parent
identity, child control, review submission, gate credit or landing authority.

See [delegation sessions](agent-delegation-sessions.md) for the surrounding
review lifecycle and [SQLite session storage](sqlite-session-storage.md) for
boot admission and the coordination reader floor.

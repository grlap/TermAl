# Persistence writer diagnostics

`GET /api/diagnostics/persistence` is a read-only, process-local observation of
the primary state writer. It returns `serverInstanceId` and a version-1
`writer` object. It acquires only the observer's mutex: no StateInner lock,
database/file reads, persistence request, or extra payload serialization.
Polling does not record anything or change a revision. Only GET is supported.

The observer retains at most 16 completed ticks, oldest first, plus the current
tick's phase and age. `completedTicks` and `failedTicks` are process-lifetime
counters. A restart resets everything; correlate with `serverInstanceId`.
Microsecond durations saturate at u64::MAX. Failed or abandoned ticks are also
retained. An abandoned scope has incomplete phase timings, not a successful
write. Empty history means no observed tick, not proven writer health.

Each tick contains `loopWallUs`, a success bit, counts and phases. The current
sample's success bit is unset/false until completion; read `current` as an
in-flight observation. Loop wall time starts after the first receive and ends
after writer bookkeeping; it excludes idle receive/retry-backoff time. It
includes draining, collection, writing, acknowledgement and tail trimming.
Do not add inclusive collection time to its wait/hold submeasurements again.

The eight existing phases are collect, serialize, open, writer ticket,
statements, commit, post-commit checks and acknowledgement. Drain and
bookkeeping times are reported separately. `collectWaitUs`, `collectHoldUs`
and `collectLockCount` measure the existing collection lock acquisitions;
the observer never holds its own mutex while waiting for StateInner.
Current phases distinguish a blocked writer from an idle one. A completed
tick preserves the existing measurements even when the write fails.

Counts describe changed/removed/deferred sessions and delegations, serialized
retained messages, and UTF-8 bytes of the already-built metadata, session
metadata, messages, prompt histories and delegation JSON. `payloadMeasured`
distinguishes unavailable measurements after early failure from zero bytes.
These are serialized input sizes, not SQL write counts, actual changed WAL
bytes, a full HTTP snapshot size, or the database's allocated size. No second
serialization or SQL query is performed to obtain the counts.

`drainedRequests` counts requests taken after the first receive, including
coalesced deltas and shutdown. `acceptedFences` counts newly accepted fences;
`pendingFencesBefore`/`pendingFencesAfter` describe the writer's accepted batch.
`newFenceAgeMaxUs` measures from fence construction immediately before send to
acceptance. Caller-side delay before send is included, so it is an upper bound
on channel wait, not an exact enqueue-to-accept duration. None of these counts
is instantaneous channel depth: `channelQueueDepth` is explicitly null because
the existing std::sync::mpsc channel does not expose it. No channel, ordering,
admission, retry, timeout, acknowledgement or SQLite setting is changed.

Observe one naturally scheduled gate and the permitted concurrent warm run,
then a quiet interval. Retain numeric aggregates and bounded failure metadata,
not complete logs or payloads. Compare collection wait/hold, serialization
against byte counts, storage/ticket phases, fence age and loop wall time.
Do not infer disk contention, a successful durability acknowledgement, or a
safe retry merely from a long tick. Live observation requires this binary to
be landed, installed and restarted by the operator under the existing rules.

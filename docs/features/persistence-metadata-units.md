# Incremental state metadata

See the [storage layout](sqlite-session-storage.md) and
[persistence architecture](../architecture.md#persistence) for the surrounding
session, transcript and writer contracts.

The metadata layout is `2-metadata-units-2`. The current primary reader floor,
`2-metadata-units-2-handoff-1`, additionally requires preserving ordinary
provider-handoff uncertainty in normalized session rows. The metadata inventory
and counts are unchanged; the units-2 rollback script refuses that stronger
floor. Federation profile versioning is independent of this local reader floor.
Growing root collections
are stored in `state_metadata_units`, with a descriptor per root field and
one row per member. Scalars, including allocation counters and the SSE
revision, have their own rows. Session transcripts, prompt history and
delegations retain their existing normalized tables.

Every supported root field has a descriptor, including empty and optional
fields. Collection descriptors contain an exact member count. Array keys
are canonical, contiguous twenty-digit ordinals; object keys retain their
original spelling. Missing descriptors, orphan members, missing tails,
unexpected fields, invalid counts and noncanonical ordinals fail closed.
Counts detect missing rows; they do not authenticate values or detect an
attacker changing both a count and its contents.

`project_metadata_units` in `src/persist_metadata_units_codec.rs` is the explicit
field-to-unit inventory. It covers federation, preferences, revision and
allocation counters, projects, source-root authority/history/audit/sighting
collections, pending coordination detachments, ignored thread IDs,
orchestrator instances, delegation waits, test-run epochs/cards/registrations/
observations/compaction/waits, layouts, the empty session carrier and Codex
state. Runtime-only fields and delegation payloads are excluded.

| Reader or writer | Authority and mapping |
| --- | --- |
| `persist.rs`: `load_state_from_sqlite_with_connection` | Startup metadata comes through schema validation and the complete unit inventory; transcripts and delegation rows load separately. |
| `persist_sqlite_schema.rs`: `validate_no_legacy_embedded_state` | Validates the assembled metadata before defaults or safety state are used; binds the loaded source to its durable predecessor. |
| `persist.rs`: `persist_delta_via_cache_inner` | Compares the captured predecessor inside writer admission; atomically writes changed units, counters, session/delegation changes and existing content markers. |
| `persist.rs`: `persist_locked_state_to_sqlite` / `persist_state_parts_via_connection` | Source-owned synchronous writes enter FIFO writer admission before capturing the locked source. They publish their own committed predecessor before admission release, including when subsequent integrity checks fail. Detached snapshots retain their immutable predecessor and cannot advance another source. |
| `persist_federation.rs`: `persist_detected_federation_quarantine` | Boot transaction reads the current image, changes its owned federation fields and updates units and federation markers together; unrelated fields remain intact. |
| `persist_federation_image.rs`: `read_federation_persist_image` | Reads only federation and preferences, checking the complete root inventory and each selected collection's count. |
| `persist_fence.rs`: `PersistFenceTarget::is_already_durable` | Uses one read transaction. Source sightings, release audit and work-authority proofs read their selected fields; session/delegation targets retain their normalized-row proofs. |
| `persist_metadata_units.rs`: `migrate_metadata_units` | Atomic, source-specific primary-2 and emergency-units-1 conversions, followed by strict units-2 validation. |

The old `app_state` metadata row remains migration provenance. Current
writers never update it. The offline rollback script reconstructs a current
primary-2 image from validated units; merely changing the version marker or
using the old row would lose newer state. Rollback requires a stopped host
and a consistent backup; the script is not an online repair operation.
Rollback refuses stores with NUL object keys before changing persistent data;
the error names the affected field. Restore from the pre-upgrade backup instead.
SQLite JSON aggregation cannot preserve those keys. Non-NUL keys, including
empty and Unicode keys, retain their exact spelling in all supported encodings.

Full and scoped metadata readers explicitly accept only primary-2, emergency
units-1 and current units-2. Missing or unknown version markers refuse before
reading any legacy provenance, including standalone federation and late-fence
readbacks that do not run startup schema setup themselves.

An immutable predecessor travels with each captured image independently of
the connection cache. A cold cache, a larger revision and replay after a
failure do not authorize replacing a newer image. A true conflict requires
incorporating the authoritative change before recapture. A successful own
COMMIT advances the source lineage conditionally; an integrity failure still
fails the receipt and discards the cache. Its old input cannot reuse the
COMMIT as a shortcut for another payload. A fresh coherent capture may
reapply the data through the normal integrity checks. No fence acknowledges
a rolled-back, conflicted or unverified write.

`metadata_units_generation` is a fixed-size concurrency token, updated with
metadata changes or coupled session/delegation mutations. An unchanged tick
with no coupled mutation changes neither units nor token. A session-only
transaction can update this small token while writing zero unit payload
bytes. The token is not a rewrite of the metadata document.

Only locally owned session/message identities contribute allocator floors.
Remote-proxy transcript IDs remain peer-owned opaque values and cannot advance
or exhaust the local allocator.

The existing persistence diagnostics endpoint reports at most 27 fixed field
names per tick, within its existing 16-tick ring. Each field reports desired
unit count and encoded bytes, attempted upserts/deletes and payload bytes,
and committed upserts/deletes and payload bytes. COMMIT success is separate
from the tick's integrity/fence result. `metadataBytes` is attempted logical
unit payload, not physical SQLite/WAL traffic; failed attempts can therefore
have nonzero attempted bytes and zero committed bytes. No values, member
keys, paths or persisted identifiers enter this telemetry.

The deterministic tests use disposable SQLite and the real capture/write/
fence/load paths. The 3,000 ignored-thread fixture is a modeled population,
not a measured production field distribution. Diagnostics describe encoded
footprint, not clone time. Source capture clones changed metadata collections
under the state lock; unit encoding runs after that lock is released. This
does not establish a CPU or latency bound. Positional arrays
preserve order and duplicates, so inserting or removing before their tail
can rewrite the affected suffix. The focused fixtures do not claim a live
physical I/O measurement or an execution of operator rollback.

Collection capture uses per-field identity and mutation-generation watermarks
in [persist_metadata_tracking.rs](../../src/persist_metadata_tracking.rs).
An unchanged collection is omitted from the captured image; the existing
writer retains its units from the verified durable predecessor. Successful
delta and synchronous full writes publish the stamps they actually captured.
The small roots `federation`, `codex`, `preferences` and `testRunCardsEpoch`
remain eager. An omitted collection cannot prove absence: late or
absence-sensitive fences use the existing scoped read-back on the committed
writer connection. No new cache, storage shape or reader floor is introduced.

Emergency units-1 did not record counts and its primary-2 migration allowed
omitted default fields. Upgrade rejects its detectable corruption, including
missing required allocation/projects/session descriptors and invalid keys,
but cannot distinguish already lost trailing rows or absent optional fields
from a valid older image. Units-2 seals the image that survives validation;
it does not reconstruct or certify missing history. Unsupported raw fields
are explicitly refused rather than silently discarded. Units-1 binaries
refuse the units-2 marker before maintenance.

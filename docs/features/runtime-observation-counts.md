# Runtime observation counts

`GET /api/diagnostics/runtime-observations` returns a count-only snapshot of
runtime observation reports retained in this host's resident sessions. It
acquires StateInner on a blocking API worker. It does not open a database,
call Engram, enqueue delivery, persist state or retry an envelope. It never
returns report/session ids, timestamps, request or receipt bodies, source
identities, connection settings or error text.

The response has `total` and `byWire` objects. `byWire` has five fixed buckets:
`contentV1`, `sourceId`, `sourceFree`, `mixed` and `unknown`. Every counter
object contains these numeric fields:

- `reports`: retained reports, including empty reports.
- `unsettledReports`: reports with at least one pending envelope.
- `localFailureReports`: reports with at least one local failure.
- `envelopes`: retained envelopes.
- `pending`, `recorded`, `refused`, `localFailure`: envelope delivery states.

Report counts and envelope counts are grouped independently. A report with
one legacy envelope and one owner-identity envelope is a `mixed` report;
its two envelopes still count under their individual formats. Source-free
envelopes are neutral when grouping a report with a source format; a report
containing only source-free envelopes is `sourceFree`. Any unknown
envelope makes its report `unknown`. An empty report is also `unknown`.
Reports may count as both unsettled and having a local failure. A local
failure is settled for retry scheduling but does not mean Engram discharged
the report.

Wire classification examines known source metadata in `execution_observe`
occurrences: source sightings, change endpoints, and optional check bases.
Legacy `content-v1:` revisions and owner `source_id` values have separate
buckets; combining both formats is `mixed`. Recognized occurrences without
source metadata are `sourceFree`. Unsupported occurrences and missing or
malformed source metadata are `unknown`. This is a wire census, not protocol
admission: it does not revalidate identity contents or traverse source paths
and changes. Arbitrary free text is never searched for source-like strings.

Counts cover only reports currently retained by this host, including those
restored on load. Discharged reports may already have been evicted. A zero
count is not proof of historical delivery, compatibility, a successful drain
or remote-host health. There is no polling worker or durable history behind
this endpoint.

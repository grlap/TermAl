# Closing-job diagnostics

`GET /api/diagnostics/closing-jobs` counts resident runtime closing jobs across
sessions. It returns the server instance and revision, terminal/retrying totals,
terminal reasons, and state counts grouped by `adoptedDay`.

`adoptedDay` is the UTC calendar day of the turn's `trace.adopted_at`, not the
day its closing attempt failed. Invalid dates are grouped under `unknown`.

Reasons are conservative classifications of stored facts: known producer
scope-change prefixes, the producer's configured-filter refusal message,
exhausted attempt counts, or `other`. Prefix matches take precedence over the
attempt limit. Retrying jobs are excluded from terminal reason counts. A legacy
message outside the recognized producer forms is not evidence of either
specific refusal; below the attempt limit it remains `other`.

This endpoint returns aggregate counts only. It does not return job error text,
identities, paths or commands, read a persistence store, retry or retire jobs,
or change their custody. It describes resident data, not a historical failure
rate or a classification of product versus environment causes.

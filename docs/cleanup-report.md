# Cleanup visibility at coordinator status

The coordinator runs this read-only report at each ordinary status sweep:

```text
node scripts/cleanup-report.mjs
```

Run it from the repository or any of its registered worktrees. It uses the
common Git directory to find the main checkout, lists registered worktrees
and top-level entries in that checkout's `.tmp/`, `.logs/` and `.evidence/`,
and reads current named roots from the host API when `TERMAL_BASE_URL` is set.
It writes JSON to stdout and creates no files. The default view lists the
first 30 entries needing review, followed by retained entries if space remains;
it includes total counts and an explicit omitted count. Use `--limit 100` for
a longer status view, or `--json` for the complete entry list.

The status report names the observation time, unresolved owners and reasons
for retention. Owner suffixes are guesses, shown separately from accountable
owners. A missing or failed host API is reported as unknown, not an empty
set of named roots. Row-level `namedRoots` is `null` when that source is
unknown; row-level `retentions` and top-level `unmatchedRetentions` are
`null` when no snapshot was supplied. Known empty sets remain arrays.
Directory sizes, dirty state, reachability, live processes,
run evidence and open-item references remain explicitly unknown: this small
command does not replace the cleanup task's detailed inventory or inspect a
tracker store. It does not walk directory contents, follow linked group
directories, run a test, delete anything or decide whether removal is safe.
An outside-repository worktree is displayed without filesystem inspection.

The coordinator may supply an existing JSON snapshot of recorded retention
needs, with an accountable owner and an event ending each need:

```text
node scripts/cleanup-report.mjs --retentions .evidence/cleanup-retentions.json
```

For example:

```json
{
  "observedAt": "2026-11-04T20:00:00Z",
  "entries": [
    {
      "path": ".evidence/open-investigation",
      "owner": "investigator",
      "reason": "original run output needed by the open investigation",
      "until": "investigation closes and durable evidence pointers are verified"
    }
  ]
}
```

The file must be a plain JSON file up to 8 MiB; do not supply a database.
Both the snapshot filename and its entry paths are resolved relative to the
main checkout and must remain inside it, including when invoked in a worktree.
The snapshot's timestamp is printed, and the coordinator rechecks that the
need remains active. An end condition never triggers automatic deletion.
An entry containing a current named root or a reported retained artifact is
labelled `retain`; a retention on a parent covers its listed children. A need
matching no listed entry is shown in `unmatchedRetentions`, even in a limited
view. An entry's accountable owner is taken only from a single retention
covering that entry, not from one artifact inside it. The artifact's owner
remains visible in the retention detail. The main checkout's owner stays unknown.
All other entries need `review`. Historical mentions,
missing references and age do not make an entry disposable. Existing path,
in-use, evidence-retention and owner-coordination checks still apply to any
later cleanup.

This is recurring visibility through the coordinator's normal status work,
with no timer or new completion gate. Owners still clean up when their work
ends, or record the concrete remaining need and its ending event. The report
does not add a second completion reminder alongside Engram's.

# Delegation inventory during startup

The server binds its local HTTP listener before reading delegation payloads.
Session and application metadata still load synchronously. A single owned
post-listen boot worker reads and decodes the full delegation table, using the
existing record validator and malformed/mismatched-row quarantine.

While that inventory is loading, requests which depend on its completeness
return HTTP 503 with `code: "delegation_inventory_starting"` and
`Retry-After: 1`. This includes session/state streams, delegation history and
state-changing routes. Health, persistence diagnostics and independent file
reads and Git queries remain available. Independent routes retain their
ordinary validation and permission checks. File writes, Git mutations and
terminal commands require the complete delegation inventory before their
existing read-only delegation guards can authorize them.

The worker prepares delegation indexes outside the state mutex. It publishes
the full records, indexes and completeness witness together, repairs child
links and uses the existing boot-recovery owner to prepare per-session
readiness before requests can observe the completed inventory. The existing
review-envelope, Engram, executor, mailbox, wait and workflow recovery sequence
then runs. Completed history is not reinterpreted as an empty inventory, and a
delegation child cannot gain root authority while its record is unloaded.
The native participation catalog and its root-description selector enforce
the same completeness fence, including on the authenticated federation
listener. The catalog child index is built with the owned records off-lock
and installed before the completeness witness becomes ready.

Loading and failed inventories preserve durable delegation rows. Unrelated
metadata/session writes cannot replace them with an empty/default snapshot;
destructive cleanup and delegation mutations remain fenced. A read snapshot
keeps the full scan consistent. If another commit occurred during it, a fresh
delegation-domain fingerprint distinguishes unrelated writes from changed
history. The final validation holds the existing persistence writer admission
and a SQLite write reservation through installation. It never waits for the
state mutex while holding those reservations, and releases them before
recovery can persist. Each SQL-admission attempt has its own existing busy
budget; time waiting for the state mutex does not consume the next attempt.
Physical file identity, the in-memory load owner and
mutation state are checked at publication. A conflicting or replaced source
is refused. The connection itself is also checked: the bundled Unix VFS's
`HAS_MOVED` checks its opened inode against its pathname, and Windows uses the
VFS's native handle to compare the captured file identity. Unix pathname and
connection checks are separate observations; repeated replacement and restore
between those observations by an external actor is a residual limitation, not
an atomic namespace lock. Renaming an in-use SQLite database is unsupported;
supported source replacement must quiesce and close existing users. Any
replacement reachable through supported TermAl operations must still be
excluded or refused. Unsupported controls and unverifiable identities fail
closed, with no pathname-only fallback. SQL iteration or row-conversion failures leave the inventory
unavailable; malformed JSON with an identifiable row ID retains quarantine.

A load failure retains an unavailable witness and logs its cause. Dependent
requests then return HTTP 503 with
`code: "delegation_inventory_unavailable"`; the empty working vector is never
published as complete. Shutdown cancels between rows and joins the owned boot
worker before the final persistence drain. No schema floor, background retry,
automatic restart or history deletion is introduced.

Startup logs distinguish `startup> listener bound elapsed_ms=…` from
`startup> delegation inventory complete rows=… elapsed_ms=…`. The first is
elapsed process boot work; the second is the worker's inventory load and
publication time. Failures use `startup> delegation inventory unavailable` or
the post-listen recovery failure line.

See [architecture](../architecture.md) for the existing delegation and boot
owners. Regression coverage is in `src/tests/delegation_startup_budget.rs` and
the existing delegation persistence, child-link, wait and recovery suites.

# Satellite onboarding

Onboarding belongs to the [central control plane](central-control-plane.md).
It adds a satellite by configuration and verification. The federation setting
starts disabled. Verification does not activate a satellite.

## Operator setup

The operator installs and configures the OpenSSH server, account, SSH keys and
TermAl binary on the satellite. Agents do not perform these acts. The operator
pins the satellite's SSH host-key fingerprint from a trusted channel and places
the pair capability in protected credential storage on both hosts. The central
TermAl host stores credential references rather than secret values.

Configure one M1 registry entry with the satellite's stable host identity,
endpoint, pinned host-key and capability references, expected protocol, schema,
OS, architecture and capacity limits. Use a stable DNS hostname (including
`.local`) for `endpoint.sshHost`, so DHCP changes do not strand the pair.
Binary hash and toolchain are recorded
diagnostics, not compatibility or delivery gates. Configure the
satellite with the matching central host identity. Enrollment creates an
Enrolled entry. Existing executor-proxy sessions are not adopted.

The execution profile's `schemaVersion` is the federation data and behavior
contract, currently `"2"`. It is versioned independently of the local SQLite
layout (`2-metadata-units-2`) and the mailbox message schema. A local storage
migration does not rewrite enrolled profiles. Once both hosts run this contract,
an existing profile `"2"` reconnects through fresh pair authentication;
it needs no profile refresh or re-enrollment. Unknown profile schemas are
refused, including a local SQLite layout identifier used as a profile schema.
Local storage migration and compatibility checks remain enforced separately.

The order, each step on the named host:

1. On the central: read `GET /api/federation` and copy its `local` identity.
2. On the satellite: configure it as a satellite of that central through
   `POST /api/federation/settings` with its current `expectedVersion`
   ([configuring a satellite](central-control-plane.md#configuring-a-satellite)).
3. On the satellite: enroll the central as its peer with
   `POST /api/federation/peers/{central}/enroll`; that record stays Enrolled
   on the satellite.
4. On the central: enroll the satellite (below).
5. Let the normal L1 connector establish the current authenticated link; no
   link record is written by hand.
6. On the satellite: read `GET /api/federation` and check its `satellite`
   block: the bound central, the central peer enrolled, and `linkConnected`.
7. On the central: request verification (below).

Settings and enrollment are not themselves proof of a link.

## Connection diagnostics

`GET /api/federation` reports each peer's existing `attestation` observation.
`currentBasis: false` and `state: "historical"` label a prior transport's
attestation; its `startupExpired` is not evidence of a new SSH attempt.
`currentBasis: null` means no attestation attempt has been recorded yet,
including when a worker is still reading credentials before its first proof.
The optional `worker` object reports the current worker ID, owning server
instance, attempt number, enrollment and transport generations, start/update
times and stage. Its own `currentBasis` identifies whether that progress
belongs to the current peer record.

`retryBackoff` includes `retryAfterMs`. `materialChanged` and
`runtimeQuarantine` identify the existing hard-fence parks; the latter also
sets `quarantinedForRuntime`. These observations do not clear those fences
or grant routing authority. `ssh` contains exit status, whether cleanup was
requested, and a fixed stderr class (`nameResolution`, `connect`,
`authentication`, `hostKey`, `forward`, or `unknown`). Classification uses
at most the first 4096 bytes while continuing to drain the pipe; raw stderr
and credentials are never returned. `stderrComplete` and `truncated` state
the limits of that evidence. A cleanup-requested exit does not establish
why the connection originally failed.

An owned child's stdout EOF or read error closes the async control input;
before a complete hello it is `linkDownBeforeHello`. An alive silent or
partial-hello child remains subject to the unchanged startup deadline.

## Explicit verification

The operator requests verification on the central TermAl host for that entry.
The local operator API uses `POST /api/federation/peers/{host_id}/enroll`
with a `record` containing the M1 fields, then
`POST /api/federation/peers/{host_id}/verify`. Retrieve the latest named report
with `GET /api/federation/peers/{host_id}/verification`. These control routes
are absent from the domain listener.
The checklist captures the complete registry basis and current link incarnation
before probes start. It uses L1's authenticated domain routes through the local
and reverse forwards. It records each check by name:

- Reachability, pinned host key and pair-capability exchange.
- TermAl version/schema and qualified host identity.
- Forward and reverse authenticated RPC round trips.
- OS, architecture, paths and process supervision in an isolated probe session,
  with a best-effort toolchain observation (`unavailable` when it cannot run).
- Wake delivery to that probe session, with a distinct durable outcome.

The probe session accepts metadata only. It cannot name a user's session,
provide a command or prompt, choose a working directory, or create an agent.
Its fixed child process checks the local supervisor and is reaped. A durable
wake acceptance and a delivered outcome remain distinct. Until the general
wake journal is available, this probe outcome uses the existing registry state
writer and its durability fence.

An unresolved acceptance on the current link blocks another probe. After an
enrollment or link change, a new explicit verification may retire that old
acceptance as `unknown` and create a new probe identity. Retirement does not
claim that the old child stopped or succeeded. Its snapshot, basis, session and
wake identities remain in a per-peer archive, with bounded late observations;
old wake identities can never launch another child or verify the new link.
The archive retains up to 16 unresolved probe records per peer without eviction.
At that limit, onboarding refuses further retirement and requires operator
reconciliation; this foundation provides no archive deletion API.

Every TermAl check must pass. Missing responses, partial results and named
failures leave the entry unverified. A configuration, identity or link change
while checks run invalidates those results; the central host cannot attach old
results to a fresh snapshot. A successful checklist moves Enrolled to Verified.
Initial activation remains a separate operator action. Changes to protocol,
schema, keys, host identity, enrollment or configuration remove prior authority
and require explicit verification or enrollment as appropriate.

Ordinary reconnects, including a restart after a hash-only rebuild, establish a
fresh authenticated pair and link incarnation using the existing pinned SSH key
and pair capability. Enrollment and membership survive. Ordinary mailbox routing
does not require an onboarding report, activation or automatic requalification.
The retained report remains diagnostic and its old proof cannot authorize a new
executor placement. Explicit verification and execution keep their separate
authority checks. Old HTTP leases remain fenced; revocation, key or enrollment
changes and continuity quarantine remain real refusals.

## Recovering a changed network address

Read `GET /api/federation`, then call the central's local operator route
`POST /api/federation/peers/{host_id}/ssh-host` with
`{"expectedVersion": <the version read>, "sshHost": "satellite.local"}`.
The request accepts only these fields and a DNS hostname, not a numeric DHCP
address. A concurrent domain change returns 409; read the registry again before
retrying. This is an address-only update, not `/enroll`: host identities,
enrollment, key references, membership, mailbox identities and history survive.
The record and transport generations advance, and the old link is withdrawn.
The existing supervisor reconnects and authenticates the new link automatically.

Each new SSH child resolves the hostname through OpenSSH. No resolved IP is
persisted or used as a durable fallback. The fixed `HostKeyAlias`, pinned
known-hosts file and `StrictHostKeyChecking=yes` still refuse a different key;
the pair proof still requires the enrolled host UUID and capability. DNS is a
locator, not proof of identity. A failed resolution stays on the existing bounded
reconnect path. Legacy IP endpoints remain readable and can be migrated through
this supported route without re-pairing or raw store edits. Free-text display
names are never guessed into hostnames.

UI session proxies have a separate locator in `preferences.remotes`. Keep that
record's `host` as the same stable hostname as well. The existing
`POST /api/settings` request accepts a `remotes` array; preserve every remote's
`id`, user, port and other settings when updating its host. This path retires
old proxy tunnels but neither changes federation enrollment nor replaces its
SSH key pins. Proxy SSH retains its own existing OpenSSH trust configuration;
updating the federation locator alone does not update the proxy locator.

The same fenced mutation preserves matching unfinished mailbox-only placements
and their INSTALL intents, rebasing only the locator and its derived local
configuration basis. Well-formed historical pairs from another identity,
enrollment, key or endpoint stay unchanged and fenced without blocking the
current peer's address update; malformed retained pairs refuse the mutation.
Executor placements, execution proof and old link incarnations are not promoted.
Published reference descriptors retain their
original exact snapshots and publication receipts; an old reference can refuse
as stale after the transition. Existing native relay messages and boot recovery
boundaries retain their original keys and sequence, and resume once the link
returns. A boot-boundary `409 federation link changed` while no link is installed
is a local deferred history read, not evidence of a failed peer identity proof.

The persisted representation and predecessor validation invariants are unchanged:
`sshHost` was already a String accepting DNS names. The paired mailbox placement
and configuration-basis update preserves the existing stored-state invariant;
this transition introduces no new reader floor or execution meaning.

The fresh identity must still match the enrolled platform and architecture.
Material identity or protocol/schema mismatches retire the link and use the
normal reconnect backoff. Temporary persistence settling returns HTTP 423 at
snapshot and mutation boundaries, including during report finalization; it
holds admission without retiring an unchanged link. Placement and onboarding
clients may retry the original intent after settling. Continuity loss or stale
authority remains HTTP 409 and cannot inherit this retry treatment.

The running image is captured by a read-only file handle at process startup.
The first enabled identity probe hashes that retained handle once, off the
state lock. Replacing the installed file before a restart cannot change the
running process's reported identity. The report records the SHA-256, size and
handle file identity (Windows volume/file index or Unix device/inode).
`pathAtStart` is diagnostic text, not the image identity. An unavailable handle
or failed hash is `unknown`; it does not prevent Verified. There is no path-based
fallback. Both directions validate observed image metadata before recording it.
Disabled federation performs no image hashing.

If Engram sync is enabled for that host/profile, invoke Engram's read-only
onboarding verification and record its named result separately. It gates Engram
sync readiness. A failed or unavailable Engram check cannot prevent TermAl
Verified or Active; a TermAl-only satellite needs no Engram check.
Automatic reconnect retains the separately recorded Engram outcome without
rerunning the external probe; it does not establish new Engram readiness.

Both host profiles explicitly opt in with the enrollment request's
`engramEnabled`. The satellite's own operator may also configure
`engramCommand`: an absolute `binary`, an argv `args` array, an absolute
`projectRoot`, and optional `jsonSummary`. Only this local configuration can
select the command; no command or project path arrives over the link. Choose
the Engram project's read-only verification entrypoint when it is available.
Until then the separately reported result is `unavailable`. TermAl interprets
only exit status and, when requested, a bounded summary shaped as
`{"passed": true, "checks": ["binary", "git"]}`. It implements no Engram checks.
An Engram version or store-readiness check proves binary/store availability;
it cannot substitute for Engram's platform and automated Git-exchange
verification in this adapter.

Repeat this configuration and checklist for a second or third satellite. No
roster or satellite count is compiled into the implementation. A platform must
also have its verified platform lane before real placement is enabled. A fake
link test proves the protocol contract; real machines remain a separate pilot.

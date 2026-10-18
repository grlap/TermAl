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
binary hash, OS, architecture, toolchain and capacity limits. Configure the
satellite with the matching central host identity. Enrollment creates an
Enrolled entry. Existing executor-proxy sessions are not adopted.

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
- OS, architecture, toolchain, paths and process supervision in an isolated
  probe session.
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
Activation remains a separate operator action. Changes to the verification
basis require verification again.

The running image is captured by a read-only file handle at process startup.
The first enabled identity probe hashes that retained handle once, off the
state lock. Replacing the installed file before a restart cannot change the
running process's reported identity. The report records the SHA-256, size and
handle file identity (Windows volume/file index or Unix device/inode).
`pathAtStart` is diagnostic text, not the image identity. An unavailable handle
or failed hash is `unknown` and cannot produce Verified; there is no path-based
fallback. Disabled federation performs no image hashing.

If Engram sync is enabled for that host/profile, invoke Engram's read-only
onboarding verification and record its named result separately. It gates Engram
sync readiness. A failed or unavailable Engram check cannot prevent TermAl
Verified or Active; a TermAl-only satellite needs no Engram check.

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

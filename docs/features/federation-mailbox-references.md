# Federation mailbox references

Part of the [central control plane](central-control-plane.md).

A mailbox reference is a fresh, caller-owned locator for an already placed and
durably bound satellite root. It is not a session, participant, remote execution
configuration or permission to install a native binding. Legacy remote proxies
are never upgraded implicitly.

The dedicated authenticated pair listener serves
`POST /federation/v1/mailbox/native-binding`. A protocol version, fresh request
UUID and expected binding are comparison inputs only. The response observes the
current installed binding, native creation token, host/central identities and
owned link in one read. Missing or conflicting bindings refuse with 409. This
operation performs no installation, mailbox, acknowledgement or runtime effect.

On the central host, an explicitly bound local root can use:

- `POST /api/sessions/{id}/mailbox-references`, with `participantId` and
  `idempotencyKey`, to provision an exact canonical remote target. Success is 201
  with `{reference: descriptor}`. An exact key replay retains its original
  target and handle; it cannot follow a successor placement or native creation.
- `GET /api/sessions/{id}/mailbox-references` to list committed descriptors.
  A descriptor is not evidence of current authority, including after reload.
- `GET /api/sessions/{id}/mailbox-references/{reference}/resolve` to obtain a
  freshly validated bare descriptor (no `{reference}` envelope). Unavailable
  transport fails visibly; there is no cached/offline success or local fallback.

Descriptors contain `id`, `kind: "federationMailboxReference"`, `generation`,
`participantId`, `nativeCreationId` and `binding`. IDs use fresh UUIDs and are
never recycled. The owner is scoped to its native creation and explicit binding.
The peer/configuration, placement and link basis are captured before network
I/O and rechecked afterward. New descriptors remain hidden through the exact
existing federation content-commit fence and a final current-basis check.
Prepared content alone is never visible, even after restart. A separate
versioned marker in the same host database binds the immutable descriptor and
its original intent. After content readback, final current-authority checks and
the small marker commit share one guarded publication cut. Database admission
and write reservation precede those guards; local contention returns retryable
`503 REFERENCE_PUBLICATION_BUSY` for the identical key. No worker/network wait
occurs under the final guards. Production content fences retain their real
five-second deadline; controlled clocks are test-only.

Failed/uncertain content persistence retains the exact hidden intent for
reconciliation and fresh validation; old prepared rows are not backfilled.
An uncertain marker commit is resolved only by exact durable lookup. Once that
cut committed, losing the response or a later link loss cannot undo it:
exact-key provisioning replay returns the same historical descriptor, even
offline, but resolve/send still require fresh current authority. A missing,
malformed, conflicting or unsupported marker cannot publish another descriptor.
An unparsable marker, including an unknown shape with extra fields, fails boot
closed; a parseable unsupported schema version remains hidden. The small marker
commit may include a local WAL checkpoint while the authority guards are held.

`termal_send_to_session` recognizes these typed handles, obtains current proof
through the resolve route, then submits the canonical participant to the
unchanged ordinary mailbox API. Even a duplicate send must resolve again.
Raw-reference HTTP sends and all executor/session routes remain unsupported.
One shared classifier reserves the ASCII-case and omitted-hyphen spellings of
the `mailbox-reference-` namespace before any name lookup, including qualified
`Host::Name` forms and test-run owner/notification aliases. Noncanonical forms
refuse; they are never silently corrected into authority. Only the exact
lowercase prefix and canonical UUID spelling can enter typed MCP resolution.
Roots may still have such display names, but must be addressed by principal id.
Reference validation does not make a disconnected placement current again.
Handles retain the exact captured peer snapshot: membership transitions or
same-link re-verification retire them, even where canonical sends remain valid.
Use a new provisioning key; an old intent never follows a changed snapshot.
Provisioning proves the binding, not central directory registration. Ordinary
sends still enforce their own current directory/placement checks and may refuse
after a successful resolve; resolving is not an atomic send or binding lease.

The local regression fixtures disclose preinstalled native bindings and
fixture readiness. They exercise loopback proof, reference and MCP paths, not
live SSH onboarding, native installation, platform supervision, full process
recovery or the complete multi-host milestone. Runtime results and independent
review are recorded separately; source descriptions alone are not proof.

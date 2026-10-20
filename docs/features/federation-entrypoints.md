# Explicit native mailbox establishment

This builds on the [federation link](federation-link.md); the
[architecture API table](../architecture.md#http-api) lists its routes.

These central operator entrypoints use an enrolled satellite's authenticated
federation link. They do not create runtimes or associate UI proxies.
Binding commands use the mutually attested link's request and response MACs;
a pair-key header alone is refused. Commands retain their 4096-byte request
and 64-KiB response limits. A lost or unverified response leaves the original
operation unresolved and its reservation retained for same-key recovery.
Once INSTALL may have committed, a later durability, authority or Register
failure also stays unresolved; retry the original operation key. A proven
pre-install conflict remains a refusal.

`POST /api/federation/peers/{id}/activate` activates only a verified peer whose
O1 report still matches its identity, configuration and live link. An identical
active result is returned without another generation change. Greg's recorded
authorization for the exact host, enrollment and verified build remains a
process prerequisite; the endpoint does not evaluate that authorization.

`POST /api/federation/peers/{id}/placements` accepts
`{ "nativeSessionId": "...", "admissionKey": "..." }`. This explicitly establishes
or recovers a mailbox placement. A fresh key requires an eligible unbound native
root. C reads its creation token through PREPARE, reserves capacity, and retains
the binding INSTALL intent in the same state transaction before sending it.
The satellite commits its binding and then Registers through the reverse link.
Its acknowledgement checks the committed federation image and the exact session
binding and native creation token on the SQLite writer's connection. A deferred
session row keeps admission held even if the metadata has already committed.

A 200 response contains `state: "installed"`, `operationKey`, `placement`,
`nativeCreationId` and `participant`. The participant's `sessionId` is the
canonical domain mailbox participant, not the reusable native session id.
A 202 response contains `state: "pending"`, the original `operationKey`,
`placement` and `nativeCreationId`; it supplies no usable participant.

Retry the same admission key and target to recover the retained binding,
creation expectation and install operation key. Recovery revalidates current
authority, verification and link; it does not run unbound-only PREPARE on an
already installed root. A successor link can recover the original intent after
fresh verification and explicit activation, preserving the domain creation id
and placement generation. Disabling federation freezes the intent without
attempting recovery. Identical material settings may resume it after fresh
verification and activation. The retained key, placement identity and generation,
execution basis, peer enrollment and verification basis, and central material
configuration basis must still match. Re-enrollment, materially changed
configuration, a recreated
native root, a changed target or conflicting binding refuse recovery. An
uncertain result never releases the reserved capacity.

The pair-authenticated satellite route
`POST /federation/v1/mailbox/binding` accepts two tagged commands:

- `prepare` with `nativeSessionId` reads an eligible unbound root's creation token.
- `install` with `binding`, `expectedNativeCreationId` and `operationKey` installs
  only that root incarnation; identical bindings are idempotent.

Read-only refresh uses the separate existing `mailbox/native-binding` proof
against a retained exact binding and placement. It does not call placement
establishment, install bindings, activate peers, or adopt a standalone root by
name, endpoint or native-id equality.

Loopback regression tests exercise two processes and real HTTP routes with fake
platform probes. They prove these entrypoints, not live Mac or O1 readiness.

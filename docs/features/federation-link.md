# Federation link foundation

This foundation connects a central TermAl host to each enrolled satellite host.
It is disabled by default through the federation settings. Standalone and
disabled hosts create no federation listeners, credential lookups or SSH children.
See the [architecture overview](../architecture.md#system-overview) for local
APIs and the existing project remote connector.

The central host initiates one OpenSSH session per satellite. An explicit local
forward reaches the satellite's dedicated federation listener; an explicit
reverse forward reaches the central host's listener for that pair. All requested
binds use loopback. Enrollment supplies the port pair; there is no discovery or
port negotiation. The session uses batch authentication, strict host-key checking,
keepalives, a pinned known-hosts file and host-key alias, and no remote command. The same process provides one owned stdio stream using
`-W` to the satellite federation listener; `ClearAllForwardings=no` preserves
both explicit forwards. No local TCP control port is opened.
User SSH configuration, multiplexing and global known-hosts files are excluded.

The OS credential service is `TermAl.Federation`. Enrollment records contain
opaque references. The pair secret is looked up as
`<local-host-id>:pair:<pair-capability-reference>`. For the central host,
`<local-host-id>:known-hosts:<known-hosts-reference>` supplies an absolute path
to the pin file; `host_key_ref` supplies its SSH alias. This transport reads
existing credentials; provisioning belongs to a separate operator workflow.

Each pair has a separate listener serving federation health, the typed
[onboarding probes](satellite-onboarding.md), the
[central mailbox protocol](central-control-plane.md), and a bounded
`TERMAL-CTRL/1` control preface. General local API and session command routes
are absent. The listener determines the enrolled caller;
an asserted host ID never selects it. The former bearer hello/link routes are
not available.

The pair key never travels in runtime headers, frames or logs. Mutual HMAC
proofs on the SSH-owned stream bind fresh 32-byte nonces, both host identities,
enrollment and central generations, both process instances, the satellite
listener address and a fresh incarnation. HKDF derives six separate request,
response and control keys. Failed proof cannot replace an established context.
Monotonic ownership guards keep delayed completion, EOF and expiry from retiring
a successor. Fresh sequenced challenge replies alone extend the 45-second
control deadline; RPC traffic and repeated heartbeats do not.

Startup has a separate 15-second budget from SSH launch through receipt of the
first complete bounded control frame. The following peer proof has a 10-second
budget, including the satellite's durable install before its final proof. The
central's own install keeps M1's separate five-second durability fence. The
runtime reads `TERMAL_FEDERATION_LINK_STARTUP_TIMEOUT_MS` and
`TERMAL_FEDERATION_ATTEST_TIMEOUT_MS` once at initialization. Each accepts
1,000 through 120,000 milliseconds; invalid values produce one warning and use
the default. Registry observations name the last stage and fixed reason.
Startup loss, EOF during proof and local install errors do not count as bad-key
refusals; parse, transcript, tag and proof-expiry refusals do.

Each HTTP message uses an incarnation/direction/counter MAC. Requests bind the
method, complete target including query, raw-body digest and semantic headers;
responses additionally bind that request, status and response metadata. The
receiver verifies the MAC before reserving a counter in a 64-position replay
window. The sender cannot allocate more than 63 positions beyond its oldest
outstanding counter. Only a matching authenticated response or refusal settles
an attempt. Cancellation, timeout, unsigned refusal or malformed response leaves
the application outcome unknown and fences the incarnation within the request
bound; application reconciliation retains the original idempotency key.

Requests and replies retain the exact enrollment, host generation and link
incarnation. Disconnect immediately withdraws that incarnation's availability;
late cleanup cannot retire its successor. Reconnect requires the unchanged
enrollment basis and clears verification. Neither disconnect nor reconnect
releases existing reservations, resolves uncertain outcomes, or grants admission.
On the federation wire, HTTP 423 identifies a temporary persistence hold;
monitors preserve the incarnation and retry after it settles. A stale-link 409
instead triggers a new bounded handshake. A connection persistence hold applies
across all pairs. The host supervisor's bounded recovery re-confirms the exact
persisted image; it never clears continuity quarantine, releases reservations
or grants admission.

RPC pressure is limited to four operations per pair. Listeners also limit
accepted connections and bound request bodies and lifetimes. Shutdown cancels
connection IO, including incomplete headers or bodies. Retries use exponential
backoff with jitter, bounded between one second and five minutes. Network and
credential IO run outside the global state lock.
The mailbox route retains its negotiated 2 MiB request and 320 MiB response
bounds; health and onboarding retain 4 KiB requests and 64 KiB responses.
A mailbox handler deadline returns an authenticated uncertain outcome while
preserving the link. Its owned blocking worker retains the admitted slot until
it settles; the caller keeps the original application idempotency key.
A signed replay-window refusal also leaves a prior mailbox delivery uncertain.
An unusable signed receipt during a local persistence hold retains that same
uncertainty and key, while settling only its transport counter.

An observed secret or pin change under the same references stops retries until
an explicit configuration transition. An unresolved OS operation or SSH cleanup
quarantines the peer for that runtime; no replacement child starts. Logs report
the peer and quarantine without secrets. Runtime registry views expose consecutive
attestation refusals without key material.
Operator quarantine reconciliation remains separate follow-up work.
This single-operator pilot authenticates the federation peer; it does not prove
exclusive local port ownership or provide confidentiality against a same-user relay.
A multi-user threat model needs the separate encrypted-channel design.

The initial validation uses fake OS boundaries and loopback links on one machine,
including two TermAl test processes. It does not establish macOS readiness or
validate real SSH provisioning.

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
keepalives, a pinned known-hosts file and host-key alias, and no remote command.
User SSH configuration, multiplexing and global known-hosts files are excluded.

The OS credential service is `TermAl.Federation`. Enrollment records contain
opaque references. The pair secret is looked up as
`<local-host-id>:pair:<pair-capability-reference>`. For the central host,
`<local-host-id>:known-hosts:<known-hosts-reference>` supplies an absolute path
to the pin file; `host_key_ref` supplies its SSH alias. This transport reads
existing credentials; provisioning belongs to a separate operator workflow.

Each pair has a separate listener mounting only `GET /federation/v1/hello`,
`POST /federation/v1/link`, and `GET /federation/v1/health`. Every request requires
the pair capability, an authority envelope and a link incarnation header. The
listener determines the enrolled caller; an asserted host ID never selects it.
The general local API, mailbox commands and session commands are absent.

A fresh handshake uses a one-use challenge bound to the receiving process.
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

An observed secret or pin change under the same references stops retries until
an explicit configuration transition. An unresolved OS operation or SSH cleanup
quarantines the peer for that runtime; no replacement child starts. Logs report
the peer and quarantine without secrets. A durable operator reconciliation view
and stronger local-forward ownership proof remain separate follow-up work.

The initial validation uses fake OS boundaries and loopback links on one machine,
including two TermAl test processes. It does not establish macOS readiness or
validate real SSH provisioning.

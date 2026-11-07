# Agent federation status

Use the read-only `termal_federation_status` MCP tool (no arguments), or:

```powershell
& $env:TERMAL_CLI federation status --json
```

The CLI uses `TERMAL_SESSION_ID` and `TERMAL_BASE_URL`, with the same root-session
boundary as the other coordination commands. `--as-session` and `--base-url`
are available explicitly. Without `--json`, the command prints formatted JSON.

The facade reads only `GET /api/federation` and
`GET /api/federation/host-utilization`. It does not contact peers itself, sample
resources, refresh data, retry a failed read, or change federation state.
See [federation links](federation-link.md) and
[host utilization](host-utilization.md) for the source contracts.
The [CLI command reference](coordination-cli.md) and
[MCP tool catalog](agent-delegation-sessions.md) also list these entry points.

The `federation` block carries the existing role, local and central identities,
membership and verification states, and attestation/worker progress. Satellite
hosts retain the existing satellite block, including its current link flag.
An absent worker remains absent. A historical observation is not converted into
a current link claim. Neither enrollment nor successful authentication means
executor activation. No running build or verified-profile hash is claimed.

The `hostUtilization` block carries the existing roster state, completeness,
host state/reason and measurement freshness. Each snapshot includes CPU busy
percent and logical count; memory total, available and in-use bytes; and
configured-volume total, free and available-to-caller bytes. Every retained
metric preserves its original value, state and reason. Top-level
`admissionQuarantine` and volume collection `failureReasons` retain their fixed
source reason codes as well. Unsupported macOS available-memory remains
unsupported; stale values remain stale. Check
`rosterState`, `complete`, host state and metric states before using a value.
Volume order is retained, but filesystem identities and paths are omitted.

`hostUtilizationStatus` is the source HTTP status when received, otherwise null.
`hostUtilizationReason` is null on success, or a fixed reason such as
`routeRefused`, `routeUnavailable`, `responseUnavailable`, `sourceLimit`,
`invalidResponse`, `invalidProjection` or `outputLimit`. An unavailable roster is null, never
an empty or complete roster. Satellites and standalone hosts normally receive
409 from this Central-only source; their federation block is still returned.
No local utilization route is substituted. Failure of the federation source
itself fails the command.

Only allowlisted status fields leave the facade. Credentials, capability
references, SSH paths, volume paths, network interfaces, toolchain strings,
control payloads and raw stderr are omitted. Each source read is limited to
18 MiB and the bridge request deadline; output is limited to 1 MiB, with at most
256 peers, 257 utilization hosts and 32 volumes per host. Oversized or invalid
data is refused rather than silently truncating a roster. The two reads are
separate observations, not an atomic snapshot or placement authorization.

# Host utilization over federation

Central can read whole-host CPU, memory, configured-volume capacity and network
counters without an SSH command. These are observations: they grant no test,
executor, placement or process-control authority. Collection never starts,
stops or modifies a process or test run.

## API

All routes are read-only GETs. Local routes use the existing local API boundary:

| Route | Result |
| --- | --- |
| `/api/host-utilization` | This host's cached snapshot |
| `/api/federation/host-utilization` | Central's cached local and enrolled-peer roster |
| `/federation/v1/host-utilization` | Snapshot on the existing authenticated pair listener |

The pair route uses the current enrolled-pair lease, incarnation, MAC and replay
protection through the existing federation client. It adds no transport,
credential, pairing or verification requirement. Health and heartbeats remain
independent of telemetry. Probe failures, old-peer 404s, oversized snapshots,
malformed snapshot data and read deadlines make telemetry unavailable; they do
not qualify or disqualify host health. Authentication, replay and authority
violations retain the existing security fences.

The aggregate is available on Central, including while peers are disconnected.
Every retained peer row carries `host` (UUID and generation), `os`,
`architecture`, `state`, `reason`, `receivedAt` and `snapshot`. Missing snapshots
are null. A local host without federation identity has a null `host`; it is not
given a fabricated persistent identity. The aggregate includes `rosterState`,
`rosterAgeMs`, `rosterTotal` and `complete`. Consumers must check these before
treating the roster as exhaustive. The finite limit is 256 peers; an overflow
reports the full enrolled count, `complete:false` and an unavailable roster,
rather than presenting the bounded rows as the full inventory. Revoked peers
are removed. Enrollment/transport generation changes discard the old cache.

Example metric fragments (all byte values are bytes, not rounded MB):

```json
{
  "schemaVersion": 1,
  "measuredAt": "2026-10-30T03:00:00.000Z",
  "sourceAgeMs": 1200,
  "staleAfterMs": 30000,
  "cpu": {
    "logicalCount": {"state":"available","value":16,"reason":null},
    "busyPercent": {"state":"available","value":25.5,"reason":null},
    "windowMs": {"state":"available","value":5000,"reason":null},
    "loadAverage1_5_15": {"state":"unsupported","value":null,"reason":"notExposedByPlatform"}
  }
}
```

Every metric has `state`, `value` and `reason`. Available and stale values are
retained; unavailable and unsupported values are null. Reasons are bounded
codes, not arbitrary OS errors or paths. A real zero is an available reading;
probe failure never becomes zero. Network collections also have this wrapper.
`volumes.data` wraps the successfully measured volume rows, whose counters have
individual metric wrappers. `volumes.complete`, `failedPathCount`,
`unavailableRequiredPathCount`, `omittedPathCountAtLeast` and `failureReasons`
describe incomplete collection without discarding good rows. Counts describe
configured paths, not invented identities or a count of unknown unique volumes.
Omission counts are bounded lower bounds when configuration limits are reached.
If no volume was measured, `volumes.data` is unavailable, never an available
empty collection. A failed required data/checkout path remains explicit even
if an optional volume was measured. Failing paths and native error strings are
not returned.

## Sampling and freshness

One owned host sampler performs non-overlapping samples, nominally every five
seconds after its previous sample completes. Slow native calls keep this one
owner; no replacement threads or queued samples accumulate. Central refreshes
each connected peer nominally every ten seconds, with at most one request per
peer and four requests globally in flight. Missed ticks coalesce. The oldest
attempt is selected first; a large roster can take longer than ten seconds.

`measuredAt` is UTC on the measuring host, at sample start. Interface counters
have their own `counterMeasuredAt` at the beginning of the network read. These
are collection timestamps, not a promise that all native counters are atomic.
`sourceAgeMs` includes collection duration. A source ages its cached sample by
its own monotonic clock. Central conservatively adds the local request round
trip and then monotonic time since receipt. It never subtracts two hosts' wall
clocks. A recent `receivedAt` does not revive a source's stale state.

After 30 seconds, available metrics become stale, with their values retained.
A disconnected or unreachable peer remains an unavailable host row with any
last-known values explicitly stale, even if they were recently received.
Unsupported metrics stay unsupported. Roster age is independent of sample age.
Roster capture continues if a native sample stalls; a busy StateInner causes a
configuration tick to be skipped, and an old roster is explicitly stale.

Cached local/pair projection and JSON encoding have a 250 ms service budget;
the aggregate has a one-second budget. Exceeding it returns 503. These budgets
do not promise scheduling on a stalled OS or bound the existing authentication
boundary or end-to-end network. Peer reads have a two-second transport budget.
Handlers perform no native probes, shell-outs, filesystem reads or fan-out,
and do not acquire StateInner. The observation cache uses a non-waiting read;
a busy cache returns 503. Native probes run outside StateInner and the async
reactor. Shutdown signals both owners and preserves completion of the bounded
authenticated peer requests. It waits at most 250 ms for the native sampler,
then detaches a still-blocked thread so the host can drain persistence. This
does not cancel the native call. The original sampler retains its occupancy
until its actual exit, including unwind; a replacement cannot start meanwhile.
A returning probe observes the pending stop before publishing or sampling again.

Snapshots are limited to 64 KiB, 64 active interfaces and 32 distinct volumes.
At most 128 configured paths and 4,096 configuration inputs are examined.
Network overflows or probe errors produce unavailable collection data. Volume
probe failures and limit omissions preserve measured rows with explicit
incompleteness and bounded reason codes. Required data and checkout paths are
examined before optional roots; limits cannot silently hide them behind the
optional roster. Only one current sample per
host is retained; there is no history or persisted telemetry database.

## Native definitions

CPU busy is a percentage of the whole host over the stated measured window;
it is not a process percentage and is not multiplied by the logical count.
The initial rate sample and a reset/decreasing counter are unavailable.

| Platform | CPU | Memory |
| --- | --- | --- |
| Windows | PDH `Processor Information(_Total)\% Processor Time`; logical count across all processor groups | Native physical total and available; in use = total − available. Standby = core + normal + reserve PDH byte counters. Free = free/zero page-list bytes. Native commit used/limit, converted from pages. Load averages and compressor RAM are unsupported. |
| Linux | Aggregate `/proc/stat`, first eight fields; idle includes iowait; guest fields are not counted twice. Logical count from online CPU rows; native 1/5/15 load averages | `/proc/meminfo` in bytes. Cache = Buffers + Cached + SReclaimable − Shmem; in use = total − free − cache. Available is the kernel's MemAvailable value; no fallback estimate. Commit and compressor fields are unsupported. |
| macOS | Mach host CPU ticks and `hw.logicalcpu`; native 1/5/15 load averages | `hw.memsize`, native free and file-backed pageable pages; in use = total − free − file-backed. Compressor is physical compressor pages. Available and Windows commit fields are unsupported. |

Cache categories reflect the platform's native accounting, not a promise that
every cached byte can be freed immediately. In particular Linux cache can
include dirty pages and reclaimable slab; macOS file-backed pageable pages can
also be dirty. `cacheDefinition` records the category. Linux MemAvailable is
the kernel's own documented estimate, transmitted as a native metric, not a
TermAl estimate. macOS free already includes speculative pages; they are not
added a second time. Compressor physical storage is not the logical size of
uncompressed pages and is not added again to the in-use residual. Windows
SystemCache includes the system working set and is deliberately not used as
standby. Checked arithmetic rejects inconsistent/overflowed counters.

Configured volumes come from the TermAl persistence directory, default checkout,
local project roots and running registered local test-run worktrees. Hosts with runner
checkouts not yet registered can set `TERMAL_UTILIZATION_VOLUMES` at startup,
using the OS path-list separator. This is configuration, not an HTTP path
parameter. Paths are resolved by native volume APIs and deduplicated by mount
or drive identity. Only those identities and capacities are returned, not
checkout paths or directory contents. Windows uses actual allocation-unit
capacity, independent of caller quota; caller-available bytes are separate.
The newer quota-independent Windows API is resolved dynamically: older systems
retain native free/caller-available counters and mark physical total unsupported.
Unix uses native block counts and distinguishes total free from caller-available
space. A configured path that cannot be resolved increments the collection's
failed-path count and marks it incomplete. Other measured volumes remain
visible. Limit omissions are counted separately with a collection-limit reason.

Network includes operationally up interfaces: Windows OperStatus Up, macOS up
and running, and Linux IFF_UP with sysfs operstate `up` or `unknown` (including
loopback and drivers without explicit operational state). Linux sysfs `flags`
does not supply the volatile IFF_RUNNING flag. It reports native cumulative
64-bit received/sent byte counters,
not packet counts, plus bytes/second from successive samples using a monotonic
window. The first sample, zero window or counter reset gives unavailable rates.
An interface disappearing or a failed enumeration discards its rate baseline.
No IP, MAC address, process list or interface secrets are exposed.

## Sources and related features

Native definitions follow [Windows performance memory](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-performance_information),
[PDH formatting and two-sample counters](https://learn.microsoft.com/en-us/windows/win32/api/pdh/nf-pdh-pdhgetformattedcountervalue),
[Windows interface counters](https://learn.microsoft.com/en-us/windows/win32/api/netioapi/ns-netioapi-mib_if_row2),
[quota-independent volume units](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/ns-fileapi-disk_space_information),
[Linux proc accounting](https://docs.kernel.org/filesystems/proc.html),
[Linux interface counters](https://docs.kernel.org/networking/statistics.html),
[Linux operational states](https://docs.kernel.org/networking/operstates.html),
and Apple's [VM statistics](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h)
and [host counter implementation](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/host.c).

See [Central control plane](central-control-plane.md),
[native mailbox relay](native-mailbox-relay.md), and
[remote full gates](remote-full-gates.md) for the existing authority and runner
workflow. Telemetry supplies information to that workflow and changes none of
its gates or permissions.

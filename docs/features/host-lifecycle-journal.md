# Host lifecycle journal

The server keeps a small, durable record of its own lifecycle, so that an exit,
a crash or a machine reset leaves evidence after the in-memory
[persistence writer diagnostics](persistence-diagnostics.md) are gone.

## File

`host-lifecycle.jsonl` sits in the TermAl state directory, beside
`termal.sqlite`. It holds one JSON object per line. When the file reaches
512 KiB it is renamed to `host-lifecycle.1.jsonl`, replacing any older one, so
the two files together stay near 1 MiB. The new file starts with a copy of the
running instance's `boot` record, so an instance that runs for days keeps its
anchor. If the rename fails, records keep going to the current file past the
bound until a later rotation succeeds. Only the server process writes the
files, under its data-directory instance lock. Each line is read on its own: a
line that cannot be read, such as a partial last line after a reset, is
skipped, and the lines around it still count.

## Records

- `boot`: the instance id, process id and the server binary's SHA-256. Written
  by the journal's own thread once the server is bound and its recovery has
  started, so hashing the binary never delays serving.
- `heartbeat`: once a minute, counters only.
  - `persistTicks` and `persistFailedTicks`: the writer's process totals.
  - `persistedPayloadBytes`: payload bytes the writer serialized since the last
    heartbeat.
  - `longestPersistTickUs`: its slowest tick since the last heartbeat.
  - `longestStateHoldMs` and `longestStateHoldAt`: the longest state-lock hold
    since the last heartbeat at or above the existing 250 ms warning threshold,
    as a source `file:line`. Null when none reached it.
  - `workingSetBytes` and `committedBytes`: the process's memory from one OS
    query. Linux and macOS report no commit.
- `outputSize`: coalesced observations of existing HTTP body bytes and each
  finalized Axum SSE frame, before compression. The 16 MiB and 64 MiB bands
  (and the reusable 1 GiB diagnostic band) report size; they impose no limit.
  Fixed producer families distinguish state, history, session, mailbox, board,
  federation, terminal, static/fallback and other routes. No URL, session id or
  payload text is retained. `complete` distinguishes a finished body/frame from
  an observed prefix at a band crossing, error or early drop;
  `counterSaturated` marks a count that is a lower bound rather than exact.
  `observationsSinceDrain` counts observations since the last drain, while
  `maxBytesObserved` and optional `maxRevisionObserved` are independent lifetime
  maxima for the fixed producer/stage/completeness/band slot. They need not
  describe the same response. A revision is included only when the producer
  supplied it; none is inferred from payloads. `nativePreWrite` is reserved for
  the native writer's observation through this same diagnostic owner.
- `panic`: written by the process panic hook before the default one runs. It
  holds the thread name, which for some threads includes a session id, the
  source location and the message. A panic message written as a literal
  keeps its first 256 bytes; a formatted message can carry runtime data, so
  only its size in bytes is written. Most standard-library panics, such as
  `expect`, format their message, so for them only the size and location are
  kept. Unlike the other records, the panic record is appended synchronously
  on the panicking thread: a panic can be the process's last act, so it cannot
  wait for the journal's thread. If another record is being appended at that
  moment, the panic record is skipped rather than waited for.
- `shutdown`: `graceful` after a normal stop, `fatal` after a server error.

Each record is one small append, flushed to disk. The heartbeat takes only the
writer observer's mutex and the state mutex's own long-hold slot, never the
state lock itself. The long-hold slot is set only on the existing
over-threshold report path, so ordinary lock use pays nothing. No message
bodies, prompts, store contents or absolute paths are written: a source
location keeps this crate's relative `src/...` path, and any other location,
such as a dependency's or the standard library's, keeps only its file name.

Output observers only update fixed saturating atomic counters. The existing
journal thread drains at most 162 slots on each heartbeat and at shutdown;
logging failure discards diagnostics without affecting delivery. The HTTP
adapter forwards the original frames, trailers, errors and size hints, without
collecting a stream, copying a body, serializing again or reserving a buffer.
Axum emits one finalized SSE event per body data frame, including keepalives,
so that frame length includes event/data prefixes, UTF-8 and line delimiters.
These measurements describe bytes yielded by the body, not socket receipt or
successful delivery to a client. A crash before the next drain can lose them.
Size records do not change previous-instance panic/shutdown classification.

## Previous instance

At boot, the server reads the journal before writing its own `boot` record. It
classifies how the instance of the last recorded boot ended, from that
instance's own records in both files:

- `shutdown`: it recorded a shutdown, with its outcome and the number of panics
  it recorded before it. A panic on a worker thread does not end the process.
- `panic`: a panic was its last record, with no heartbeat or shutdown after
  it. The result also carries the boot time, the last heartbeat time and the
  number of panics.
- `unclean`: no shutdown, and if it panicked, it kept running afterwards. It
  was killed, crashed natively, or the machine reset. The result carries the
  boot time, the last heartbeat time and the number of panics.
- `unknown`: no earlier boot is recorded.

`GET /api/diagnostics/host-lifecycle` returns `serverInstanceId`, the journal
file name and that `previousInstance` classification, which stays null until
the journal's thread has read it. It reads no file, state or database.

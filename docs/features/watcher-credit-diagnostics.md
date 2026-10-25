# Watcher credit diagnostics

A source-file notification can refuse a carried test's verification even when
its final bytes and modification time match their initial values. A transient
write and restore, a metadata change, or a delayed notification can all have
that shape. Matching endpoint hashes do not identify the writer or prove that
no write occurred. Refusals remain conservative, including notifications handled
after the run's terminal record was read.

The first notification that fences a check retains a separate bounded
`watcher provenance` diagnostic on that check and logs it once, after releasing
the state mutex. The existing refusal cause and bounded holder notice retain
their original text. The diagnostic includes:

- The raw Notify event kind, tracker, rescan flag and path count, before the
  event is converted into the UI's broader change kind.
- `writer_pid` only when supplied by Notify, otherwise `unknown`. A supplied
  value is a backend report, not an independently verified writer identity.
  Backend info and source strings are represented only by presence flags;
  their contents are not copied into the diagnostic.
- Process-local ordering ticks for notification receipt (`observed`), watcher
  handling (`processed`), conversion completion (`converted`), routing
  completion (`delivered`) and check fencing (`fenced`). `observed_at` is the
  host's UTC receipt time. `queue_us` measures receipt-to-handling elapsed time,
  excluding conversion and attribution work. These
  observations do not reveal when the filesystem write occurred; delivery can
  itself be delayed before the callback runs.
- A short routed-root spelling and its full SHA-256, plus hashes of the exact
  runtime check key and grant, check sequence, and recorded command-start time.
  A hash of the original refusal cause links the records without repeating
  its potentially long path. A command fingerprint alone can recur across
  different runs.

Ingress without a raw notification explicitly reports its kind, observation
time and writer as unknown. Routing overflow keeps its existing conservative
unknown-root behavior. Existing scratch and Cargo sibling handling is unchanged.
The diagnostic adds no event exemption, source-hash waiver, or writer inference.

There is no additional event history or persisted schema. Each check keeps
only its first refusal and one provenance record of at most 1400 bytes; raw
provenance has a fixed field set, backend text is omitted, and routed-root
display is truncated with a full digest kept. Formatting reads no files under
the state mutex, and diagnostic logging happens after unlocking. The provenance
does not consume the existing merged-notice budget or evict earlier notices.

# Emits a doctor report far larger than the OS pipe buffer, then exits.
# This is the discriminator for draining: while the host waited for exit before
# reading, this child blocked on its own write and never exited, so the call
# ended at the deadline and reported a slow audit. With the streams drained on
# their own threads it completes and parses.
#
# 120 KB of padding sits well above the ~64 KiB buffer measured on Windows and
# well below the doctor-specific read cap. The padding field is ignored by serde, so the
# payload still deserializes as EngramDoctorResult.
$padding = 'x' * 120000
[Console]::Out.Write('{"healthy":true,"database":"C:\\termal-doctor-fixture\\engram.sqlite","project_id":"termal-doctor-large","padding":"' + $padding + '"}')

#!/bin/sh
set -eu
# Emits a doctor report far larger than the OS pipe buffer, then exits.
# This is the discriminator for draining: while the host waited for exit before
# reading, this child blocked on its own write and never exited, so the call
# ended at the deadline and reported a slow audit. With the streams drained on
# their own threads it completes and parses.
#
# 120 KB of padding sits well above the typical 64 KiB pipe buffer and well
# below the doctor-specific read cap. The padding field is ignored by serde, so the
# payload still deserializes as EngramDoctorResult.
padding=$(head -c 120000 /dev/zero | tr '\0' 'x')
printf '%s' '{"healthy":true,"database":"/tmp/termal-doctor-fixture/engram.sqlite","project_id":"termal-doctor-large","padding":"'
printf '%s' "$padding"
printf '%s' '"}'

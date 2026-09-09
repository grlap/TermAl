# Stands in for an `engram doctor --json` run that outlasts its deadline and
# then SUCCEEDS. The success matters: it is what makes the test discriminating.
# With an already expired deadline the host must kill this process on the first
# poll and report the timeout; if the deadline or the expiry branch regressed,
# the call would instead parse the payload below and return Ok, so the test
# fails rather than merely returning late.
#
# The payload must deserialize as EngramDoctorResult - healthy, database and
# project_id are all required with no serde defaults. Two seconds is long
# enough to outlast any deadline the test sets and short enough that a stray
# child cannot linger.
Start-Sleep -Seconds 2
[Console]::Out.Write('{"healthy":true,"database":"C:\\termal-doctor-fixture\\engram.sqlite","project_id":"termal-doctor-fixture"}')

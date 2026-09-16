$ErrorActionPreference = 'Stop'
# Beads (bd) read fixture: records argv (last command and a full log), refuses
# anything that is not a --readonly --json read, and answers list / show /
# comments with bd 1.2.2 receipt shapes (every list row carries its edges
# inline; `show` prints an array even for one id).
$args | Set-Content -LiteralPath (Join-Path (Get-Location) 'beads-read-args.txt')
Add-Content -LiteralPath (Join-Path (Get-Location) 'beads-read-args-log.txt') -Value ($args -join ' ')
"$env:BEADS_DIR|$env:BEADS_DB" | Set-Content -LiteralPath (Join-Path (Get-Location) 'beads-read-env.txt')
if (($args[0] -ne '--readonly') -or ($args[1] -ne '--json')) { [Console]::Error.WriteLine('fixture requires --readonly --json first'); exit 9 }
$rest = @($args | Select-Object -Skip 2)
if ($rest[0] -eq 'memories') { [Console]::WriteLine('{"schema_version":1,"guide":"Summary\nFull body"}'); exit 0 }
if ($rest[0] -eq 'recall') { [Console]::WriteLine('{"schema_version":1,"key":"guide","found":true,"value":"Summary\nFull body"}'); exit 0 }
$rootRow = '{"id":"tm-root","title":"Root epic <script> inert","description":"Epic","status":"in_progress","priority":2,"issue_type":"feature","assignee":"Termal::Codex","owner":"Greg","created_at":"2026-09-01T00:00:00Z","created_by":"Greg","updated_at":"2026-09-13T00:00:00Z","started_at":null,"dependency_count":0,"dependent_count":1,"comment_count":2}'
if ($rest[0] -eq 'list') {
  if (Test-Path -LiteralPath 'beads-fixture-fail') { [Console]::Error.WriteLine('Error: database is locked'); exit 1 }
  if (Test-Path -LiteralPath 'beads-fixture-malformed') { [Console]::WriteLine('not json'); exit 0 }
  if ($rest -contains '--label=decision') { [Console]::WriteLine('[' + $rootRow + ']'); exit 0 }
  if (@($rest | Where-Object { $_ -like '--label=*' }).Count -gt 0) { [Console]::WriteLine('[]'); exit 0 }
  # tm-root.1 declares two blockers (dependency_count counts `blocks` only,
  # as bd 1.2.2 does) and carries its records inline: two blocks and the
  # parent-child edge. Markers: no-edges drops the records and the parent;
  # partial-edges keeps only the parent-child record; two-dependents makes
  # tm-free declare and carry one blocker of its own.
  $childDeps = ',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-free","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"}],"parent":"tm-root"'
  if (Test-Path -LiteralPath 'beads-fixture-no-edges') { $childDeps = '' }
  if (Test-Path -LiteralPath 'beads-fixture-partial-edges') { $childDeps = ',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"}],"parent":"tm-root"' }
  # external-blocker: a third blocker whose reference is not a Beads
  # identifier (a cross-project reference), declared and carried.
  $childCount = 2
  if (Test-Path -LiteralPath 'beads-fixture-external-blocker') { $childCount = 3; $childDeps = ',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-free","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"external:other:tm-1","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"}],"parent":"tm-root"' }
  # malformed-record: the full records plus one that does not parse (no type).
  if (Test-Path -LiteralPath 'beads-fixture-malformed-record') { $childDeps = ',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-free","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-odd"}],"parent":"tm-root"' }
  $freeCount = 0; $freeDeps = ''
  if (Test-Path -LiteralPath 'beads-fixture-two-dependents') { $freeCount = 1; $freeDeps = ',"dependencies":[{"issue_id":"tm-free","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-03T00:00:00Z","created_by":"Greg","metadata":"{}"}]' }
  [Console]::WriteLine('[' + $rootRow + ',{"id":"tm-root.1","title":"Blocked child","description":"Waits","status":"open","priority":1,"issue_type":"task","assignee":null,"owner":"Greg","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","updated_at":"2026-09-12T00:00:00Z","started_at":null,"dependency_count":' + $childCount + ',"dependent_count":0,"comment_count":0' + $childDeps + '},{"id":"tm-free","title":"Ready bug","description":"","status":"open","priority":3,"issue_type":"bug","assignee":null,"owner":"Greg","created_at":"2026-09-03T00:00:00Z","created_by":"Greg","updated_at":"2026-09-11T00:00:00Z","started_at":null,"dependency_count":' + $freeCount + ',"dependent_count":1,"comment_count":0' + $freeDeps + '}]')
  exit 0
}
if ($rest[0] -eq 'show') {
  if (Test-Path -LiteralPath 'beads-fixture-malformed-show') { [Console]::WriteLine('not json'); exit 0 }
  if (Test-Path -LiteralPath 'beads-fixture-fail-show') { [Console]::Error.WriteLine('Error: database is locked'); exit 1 }
  # An unknown-id line beside a store failure: not a refusal of unknown ids.
  if (Test-Path -LiteralPath 'beads-fixture-mixed-refusal-show') { [Console]::Error.WriteLine('Error fetching tm-missing: no issue found matching "tm-missing"'); [Console]::Error.WriteLine('Error: database is locked'); exit 1 }
  if (Test-Path -LiteralPath 'beads-fixture-same-line-refusal-show') { [Console]::Error.WriteLine('Error fetching tm-missing: no issue found matching "tm-missing" (database is locked)'); exit 1 }
  if (Test-Path -LiteralPath 'beads-fixture-aggregate-refusal-show') { [Console]::Error.WriteLine('Error fetching tm-missing: no issue found matching "tm-missing"'); [Console]::Out.WriteLine("{`n  `"error`": `"no issues found matching the provided IDs`",`n  `"schema_version`": 1`n}"); exit 1 }
  # The unknown-id refusal as a JSON error on stdout beside an unrelated
  # stderr notice: the classification must read both streams.
  if (Test-Path -LiteralPath 'beads-fixture-unknown-show-json') { [Console]::Error.WriteLine('warning: a newer bd is available'); [Console]::WriteLine('{"error":"Error fetching tm-missing: no issue found matching \"tm-missing\"","schema_version":1}'); exit 1 }
  # bd 1.2.2 semantics: unknown ids are omitted from the receipt (with a
  # stderr line each); a receipt with no known id exits 1; the receipt is an
  # array even for one id. `tm-root` resolves to the tm-root.1 object, like
  # a prefix/alias lookup returning another issue.
  $closed = '{"id":"tm-closed","title":"Closed blocker","description":"","status":"closed","priority":2,"issue_type":"task","assignee":null,"owner":"Greg","created_at":"2026-08-01T00:00:00Z","created_by":"Greg","updated_at":"2026-08-02T00:00:00Z","started_at":null,"dependent_count":1,"dependency_count":0,"comment_count":0}'
  $child = '{"id":"tm-root.1","title":"Blocked child","description":"Waits for <b>inert</b>","status":"open","priority":1,"issue_type":"task","assignee":null,"owner":"Greg","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","updated_at":"2026-09-12T00:00:00Z","started_at":null,"dependencies":[{"id":"tm-root","title":"Root epic <script> inert","status":"in_progress","priority":2,"issue_type":"feature","dependency_type":"parent-child"},{"id":"tm-free","title":"Ready bug","status":"open","priority":3,"issue_type":"bug","dependency_type":"blocks"}],"parent":"tm-root","dependent_count":0,"dependency_count":2,"comment_count":1}'
  $ids = @($rest | Select-Object -Skip 1)
  $found = @()
  foreach ($id in $ids) {
    if ($id -eq 'tm-closed') { $found += $closed }
    elseif (($id -eq 'tm-root.1') -or ($id -eq 'tm-root')) { $found += $child }
    else { [Console]::Error.WriteLine("Error fetching ${id}: no issue found matching `"${id}`"") }
  }
  if ($found.Count -eq 0) { exit 1 }
  [Console]::WriteLine('[' + ($found -join ',') + ']')
  exit 0
}
if ($rest[0] -eq 'comments') {
  # Marker: a comment recorded for another issue than the one requested.
  $issue = if (Test-Path -LiteralPath 'beads-fixture-stray-comment') { 'tm-other' } else { 'tm-root.1' }
  [Console]::WriteLine('[{"id":"c-1","issue_id":"' + $issue + '","author":"Greg Lapinski","text":"First comment <i>inert</i>","created_at":"2026-09-12T10:00:00Z"}]')
  exit 0
}
[Console]::Error.WriteLine('fixture: unsupported operation')
exit 7

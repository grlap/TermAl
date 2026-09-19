$ErrorActionPreference = 'Stop'
if ($args -contains 'control-policy') {
  [Console]::WriteLine((Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'engram-control-policy-show.json')))
  exit 0
}
$fixtureHomeIndex = [Array]::IndexOf($args, '--home')
if ($fixtureHomeIndex -lt 0 -or $fixtureHomeIndex + 1 -ge $args.Length) {
  [Console]::Error.WriteLine('fixture requires --home'); exit 2
}
$fixtureHome = $args[$fixtureHomeIndex + 1]
$args | Set-Content -LiteralPath (Join-Path $fixtureHome 'work-read-args.txt')
if ($args -contains 'memories') {
  if ($args -contains '--full') { [Console]::WriteLine('{"key":"guide","revision":2,"body":"<script>inert</script>","remembered_at":"2026-09-16T00:00:00Z","actor_id":"test/termal"}'); exit 0 }
  if ($args -contains '--after=guide') { [Console]::WriteLine('{"memories":[{"key":"later","revision":1,"first_line":"Later memory","remembered_at":"2026-09-16T00:00:00Z","actor_id":"test/termal"}],"omitted_count":0,"exhausted":true}'); exit 0 }
  [Console]::WriteLine('{"memories":[{"key":"guide","revision":2,"first_line":"Summary","remembered_at":"2026-09-16T00:00:00Z","actor_id":"test/termal"}],"next_after":"guide","omitted_count":0,"exhausted":false}'); exit 0
}
if ($args -contains '--search=stale') { [Console]::Error.WriteLine('work_catalog_cursor_invalid: fixture changed'); exit 1 }
if ($args -contains '--search=malformed') { [Console]::WriteLine('not json'); exit 0 }
if ($args -contains 'show') {
  if ($args -contains '--after=stale') { [Console]::Error.WriteLine('work_show_cursor_invalid: fixture changed'); exit 1 }
  $fixtureStatus = '"status":{"work":{"short_ref":"w-test","title":"Fixture","outcome":"Goal","acceptance":[],"priority":2,"kind":"bug","lifecycle":"open"},"availability":"ready"},'
  if ($args -contains '--after=older') { $fixtureStatus = '' }
  [Console]::WriteLine(('{' + $fixtureStatus + '"notes":[],"notes_window":{"total":0,"shown":0,"newer":0,"older":0,"read_cut":{"project_position":1,"observed_at":"now"}}}'))
  exit 0
}
[Console]::WriteLine('{"items":[{"work":{"work_id":"fixture-id","short_ref":"w-test","title":"Fixture","kind":"bug","lifecycle":"open","priority":2,"labels":[],"assigned_to":null,"parent_id":null,"updated_at":"2026-09-13T00:00:00Z"},"availability":"ready","blocked_by":[]}],"total":1,"shown_before":0,"more":false}')

# Owns doctor descendant/pipe-lifetime fixtures; no real Engram store is opened.
$ErrorActionPreference = "Stop"
$projectFile = ""
$engramHome = ""
for ($i = 0; $i -lt $args.Count; $i++) {
    if ($args[$i] -eq "--project-file") { $projectFile = $args[$i + 1] }
    if ($args[$i] -eq "--home") { $engramHome = $args[$i + 1] }
}
$info = [System.Diagnostics.ProcessStartInfo]::new()
$info.FileName = "powershell.exe"
$childScript = Join-Path $engramHome "engram-descendant.ps1"
$info.Arguments = "-NoLogo -NoProfile -NonInteractive -File `"$childScript`""
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.RedirectStandardOutput = $true
# stderr deliberately remains inherited from the doctor.
$child = [System.Diagnostics.Process]::Start($info)
do {
    $line = $child.StandardOutput.ReadLine()
    if ($null -eq $line) { throw "descendant exited before readiness" }
} while (-not $line.Contains("termal-descendant-ready"))
[Console]::Out.Write('{"healthy":true,"database":"C:\\fixture\\engram.sqlite","project_id":"doctor-pipe"}')
[Console]::Out.Flush()
if ((Get-Content -LiteralPath $projectFile -Raw).Trim() -eq "doctor-tree-hang") {
    $child.WaitForExit()
}
exit 0

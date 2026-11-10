$ErrorActionPreference = "Stop"

$projectFile = $null
$engramHome = $null
$actorId = $null
$actorContext = $null
$sessionId = $null
for ($index = 0; $index -lt $args.Count - 1; $index += 1) {
    if ($args[$index] -eq "--project-file") {
        $projectFile = $args[$index + 1]
    }
    if ($args[$index] -eq "--home") {
        $engramHome = $args[$index + 1]
    }
    if ($args[$index] -eq "--actor-id") {
        $actorId = $args[$index + 1]
    }
    if ($args[$index] -eq "--actor-context") {
        $actorContext = $args[$index + 1]
    }
    if ($args[$index] -eq "--session-id") {
        $sessionId = $args[$index + 1]
    }
}
if (-not $projectFile -or -not $engramHome) {
    exit 2
}
# This fixture writes a placeholder store and marker files under --home. Every
# TermAl test temp root lives below <user temp>\termal\tests, with the user
# temp derived as test_temp_dir() derives it: TERMAL_TEST_USER_TEMP, else the
# platform temp directory (GetTempPath here, GetTempPath2 in Rust; the two
# differ only for a SYSTEM-account process, where only the launcher's
# TERMAL_TEST_USER_TEMP keeps them aligned). Any other home is a real Engram
# home (a developer's ~/.engram received one once). Refuse before any write; exit 4
# is used by no other exit in either fixture script. Containment is checked on
# the normalised absolute path at a component boundary, and any `..` segment is
# refused outright so the check never leans on normalisation. Case-insensitive
# comparison is deliberate: this script only runs on Windows. A \\?\-prefixed
# home is refused too (GetFullPath leaves it unnormalised; TestTempRoot never
# produces one).
$userTemp = if ($env:TERMAL_TEST_USER_TEMP) { $env:TERMAL_TEST_USER_TEMP } else { [System.IO.Path]::GetTempPath() }
$testRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($userTemp, 'termal', 'tests'))
$resolvedHome = [System.IO.Path]::GetFullPath($engramHome)
$homeContained = (
    $resolvedHome.Equals($testRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
    $resolvedHome.StartsWith($testRoot + '\', [System.StringComparison]::OrdinalIgnoreCase)
)
$rawHomeRefused = (($engramHome -split '[\\/]') -contains '..') -or $engramHome.StartsWith('\\?\')
if (-not $homeContained -and -not $rawHomeRefused) {
    # TestTempRoot returns the final directory spelling. GetFullPath cannot
    # resolve a junction or an 8.3 ancestor in the user-temp spelling. Keep the
    # ordinary lexical path fast; only this alias comparison opens metadata
    # handles, never creates a directory or changes production path policy.
    try {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class TermAlFixtureDirectoryPath {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(string path, uint access,
        uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandleW(SafeFileHandle handle,
        StringBuilder path, uint capacity, uint flags);
    public static string Resolve(string path) {
        string extended = path.StartsWith(@"\\")
            ? @"\\?\UNC\" + path.Substring(2) : @"\\?\" + path;
        // Metadata-only, shared read/write/delete, existing directory only.
        using (var handle = CreateFileW(extended, 0, 7, IntPtr.Zero, 3,
                                      0x02000000, IntPtr.Zero)) {
            if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            var result = new StringBuilder(512);
            uint size = GetFinalPathNameByHandleW(handle, result, (uint)result.Capacity, 0);
            if (size >= result.Capacity && size < 32768) {
                result = new StringBuilder((int)size + 1);
                size = GetFinalPathNameByHandleW(handle, result, (uint)result.Capacity, 0);
            }
            if (size == 0 || size >= result.Capacity)
                throw new Win32Exception(Marshal.GetLastWin32Error());
            string final = result.ToString();
            return final.StartsWith(@"\\?\UNC\") ? @"\\" + final.Substring(8)
                : final.StartsWith(@"\\?\") ? final.Substring(4) : final;
        }
    }
}
'@
        $finalRoot = [TermAlFixtureDirectoryPath]::Resolve($testRoot)
        $finalHome = [TermAlFixtureDirectoryPath]::Resolve($resolvedHome)
        $homeContained = (
            $finalHome.Equals($finalRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
            $finalHome.StartsWith($finalRoot + '\', [System.StringComparison]::OrdinalIgnoreCase)
        )
    } catch {
        # An unavailable final path supplies no permission to write.
        $homeContained = $false
    }
}
if ($rawHomeRefused -or -not $homeContained) {
    [Console]::Error.WriteLine("engram-control-fixture: refusing --home outside a TermAl test temp root ($testRoot): $engramHome")
    exit 4
}

$workIndex = [Array]::IndexOf($args, "work")
if ($workIndex -ge 0 -and ($args -contains "next")) {
    if (-not $env:ENGRAM_HOME -or -not $env:ENGRAM_ACTOR_ID -or -not $env:ENGRAM_ACTOR_CONTEXT -or -not $env:ENGRAM_SESSION_ID) {
        [Console]::Error.WriteLine("missing Engram base context environment")
        exit 8
    }
    if ($env:ENGRAM_HOME -ne $engramHome -or $env:ENGRAM_ACTOR_ID -ne $actorId -or $env:ENGRAM_ACTOR_CONTEXT -ne $actorContext -or $env:ENGRAM_SESSION_ID -ne $sessionId) {
        [Console]::Error.WriteLine("Engram base context argv/environment mismatch")
        exit 9
    }
    [System.IO.File]::AppendAllText((Join-Path $engramHome "work-context-reads"), "next`n")
    $args | ConvertTo-Json -Compress | Set-Content -LiteralPath (Join-Path $engramHome "work-context-args.json")
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    if ($mode -eq "fixture-work-next-delivery") {
        # Model the documented peek/next boundary, independently of host cache.
        $cursorPath = Join-Path $engramHome "work-delivery-cursor"
        $page = if (Test-Path -LiteralPath $cursorPath) { [int](Get-Content -LiteralPath $cursorPath -Raw) } else { 0 }
        if (-not ($args -contains "--peek")) {
            [System.IO.File]::WriteAllText($cursorPath, [string]($page + 1))
        }
        [Console]::Out.WriteLine("delivery-page-$page")
        [Console]::Out.WriteLine("x" * 40000)
        [Console]::Out.WriteLine("complete-page-tail")
        exit 0
    }
    if ($mode -eq "fixture-work-next-gated" -and
        -not (Test-Path -LiteralPath (Join-Path $engramHome "work-context-released"))) {
        & (Join-Path $engramHome "work-context-gate.cmd") > $null
        if ($LASTEXITCODE -ne 0) { throw "work-context release helper failed" }
    }
    [Console]::Out.WriteLine("Engram work context for $($env:ENGRAM_SESSION_ID) as $($env:ENGRAM_ACTOR_ID)")
    exit 0
}

if (($args -contains "doctor") -or ($args -contains "readiness")) {
    $diagnostic = if ($args -contains 'readiness') { 'readiness' } else { 'doctor' }
    [IO.File]::AppendAllText((Join-Path $engramHome 'diagnostic-commands'), "$diagnostic`n")
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    if ($mode -eq 'fixture-diagnostic-large-refusal') {
        [Console]::Out.WriteLine('invalid-json-' + ('x' * 100000))
        [Console]::Error.WriteLine('warning-' + ('y' * 100000))
        exit 1
    }
    if ($mode -eq 'fixture-diagnostic-marker-oversized') {
        [IO.File]::WriteAllText($projectFile, ('x' * 4097))
    }
    if (($args -contains 'readiness') -and $mode -eq 'fixture-readiness-old') { [Console]::Error.WriteLine('unknown subcommand readiness'); exit 2 }
    if (($args -contains 'readiness') -and $mode -eq 'fixture-readiness-malformed') { [Console]::Out.WriteLine('{}'); exit 0 }
    if (($args -contains 'readiness') -and $mode -eq 'fixture-readiness-refusal') { [Console]::Out.WriteLine('{"ready":true}'); exit 1 }
    # Reports the temporary directory this child was given, which is where
    # PowerShell writes its startup policy probe.
    if (($args -contains 'readiness') -and $mode -eq 'fixture-report-temp') { [Console]::Out.WriteLine("TEMP=$env:TEMP"); [Console]::Out.WriteLine("TMP=$env:TMP"); exit 0 }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $hash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($mode))).Replace('-', '').ToLowerInvariant()
    $sha.Dispose()
    $database = [System.IO.Path]::GetFullPath((Join-Path $engramHome "projects/$hash/engram.db"))
    # Hashed store paths can exceed Windows PowerShell's legacy 248-character
    # directory limit inside the contained test run root. Match native Rust
    # filesystem handling without shortening or changing the store identity.
    $ioDatabase = if ($database.StartsWith('\\?\')) { $database } elseif ($database.StartsWith('\\')) { '\\?\UNC\' + $database.Substring(2) } else { '\\?\' + $database }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($ioDatabase)) | Out-Null
    if (-not [IO.File]::Exists($ioDatabase)) { [IO.File]::WriteAllText($ioDatabase, 'fixture database') }
    if ($mode -eq 'fixture-diagnostic-large-report') {
        [Console]::Error.WriteLine('warning-' + ('y' * 100000))
        [Console]::Out.WriteLine((@{
            healthy = $true; database = $database; project_id = $mode
            detail = ('x' * 100000)
        } | ConvertTo-Json -Compress))
        exit 0
    }
    if ($args -contains "readiness") {
        [Console]::Out.WriteLine((@{
            schema_version = 1; scope = 'readiness'; ready = $true
            full_audit = 'not_run'; mutation_enabled = $false; work_schema_version = 1
            host_path_policy = @{ stored = 'fixture'; resolved = 'fixture'; status = 'matched' }
            database = $database; project_id = $mode
        } | ConvertTo-Json -Compress -Depth 10))
        exit 0
    }
    [Console]::Out.WriteLine((@{
        healthy = ($mode -ne 'fixture-audit-unhealthy')
        database = $database
        project_id = $mode
    } | ConvertTo-Json -Compress -Depth 10))
    if ($mode -eq 'fixture-audit-unhealthy') { [Console]::Error.WriteLine('development redactor provides no protection'); exit 1 }
    exit 0
}

if (-not $actorId -or -not $sessionId -or
    $env:ENGRAM_HOME -ne $engramHome -or
    $env:ENGRAM_ACTOR_ID -ne $actorId -or
    $env:ENGRAM_ACTOR_CONTEXT -ne $actorContext -or
    $env:ENGRAM_SESSION_ID -ne $sessionId) {
    [Console]::Error.WriteLine("Engram control argv/environment mismatch")
    exit 10
}

function Write-Result($result) {
    [Console]::Out.WriteLine((@{ status = "ok"; result = $result } | ConvertTo-Json -Compress -Depth 10))
    [Console]::Out.Flush()
}

function Write-ControlError([string] $code, [string] $message) {
    [Console]::Out.WriteLine((@{
        status = "error"
        error = @{ code = $code; message = $message }
    } | ConvertTo-Json -Compress -Depth 10))
    [Console]::Out.Flush()
}

# Tree fixtures publish startup only after the test owns the descendant's
# process handle. Cold process startup is not part of a request deadline.
$initialMode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
if ($initialMode.StartsWith("fixture-tree-")) {
    $descendantPath = Join-Path $engramHome "engram-descendant.ps1"
    if ($initialMode -eq "fixture-tree-eof") {
        # Shell launch avoids inheriting extra control-pipe handles on Windows.
        # The test awaits the descendant's TCP publication before requesting EOF.
        Start-Process -FilePath "powershell.exe" -ArgumentList @(
            "-NoLogo", "-NoProfile", "-NonInteractive", "-File", $descendantPath
        ) -WindowStyle Hidden
    } else {
    $processInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $processInfo.FileName = "powershell.exe"
    $processInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -File `"$descendantPath`""
    $processInfo.UseShellExecute = $false
    $processInfo.CreateNoWindow = $true
    $processInfo.RedirectStandardOutput = $true
    $descendantProcess = [System.Diagnostics.Process]::Start($processInfo)
    do {
        $readyLine = $descendantProcess.StandardOutput.ReadLine()
        if ($null -eq $readyLine) { throw "descendant exited before readiness" }
    } while (-not $readyLine.Contains("termal-descendant-ready"))
    }
}
if ($initialMode.StartsWith("fixture-stderr-")) {
    [IO.File]::WriteAllText((Join-Path $engramHome "stderr-child-pid"), [string]$PID)
    [Console]::Error.Write("thread 'fixture' panicked at PRIVATE_PATH token=PRIVATE_TOKEN PRIVATE_REQUEST " + ("x" * 131072))
    [Console]::Error.Flush()
    if ($initialMode -eq "fixture-stderr-startup-exit") { exit 0 }
}
[Console]::Out.WriteLine("termal-engram-control-fixture-ready")
[Console]::Out.Flush()

while ($null -ne ($line = [Console]::In.ReadLine())) {
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    if ($mode -eq "fixture-stderr-exit") { exit 0 }
    if ($mode -eq "fixture-eof") {
        exit 0
    }
    if ($mode -eq "fixture-hang" -or $mode -eq "fixture-stderr-hang") {
        # No response and no timer: the host must kill this blocked reader.
        [void][Console]::In.ReadLine()
        exit 0
    }
    if ($mode.StartsWith("fixture-tree-")) {
        if ($mode -eq "fixture-tree-eof") {
            exit 0
        }
        if ($mode -eq "fixture-tree-reply") {
            $request = $line | ConvertFrom-Json
            if ($request.operation -eq "work_session_read") {
                # Framing-only transport response, not an identity or capability proof.
                Write-Result @{ binding = $null }
            } else {
                Write-ControlError "invalid_request" "unsupported fixture operation"
            }
            continue
        }
        [void][Console]::In.ReadLine()
        exit 0
    }
    if ($mode -eq "fixture-malformed") {
        [Console]::Out.WriteLine('{"status":')
        [Console]::Out.Flush()
        continue
    }
    # Terminal control-startup replies: one flushed error line answering the
    # frame, then a nonzero exit, with no dispatch.
    if ($mode -eq "fixture-startup-busy") {
        Write-ControlError "control_startup_busy" "Engram control startup could not acquire the store."
        exit 3
    }
    if ($mode -eq "fixture-startup-refused") {
        Write-ControlError "control_startup_refused" "Engram control startup was refused."
        exit 3
    }
    # Request-level error replies on a healthy connection, which continues.
    if ($mode -eq "fixture-remote-storage-error") {
        Write-ControlError "storage_error" "COMMIT or rollback failed"
        continue
    }
    if ($mode -eq "fixture-remote-unknown-code") {
        Write-ControlError "fixture_future_code" "a code this host does not know"
        continue
    }
    $request = $line | ConvertFrom-Json
    $rootRead = Join-Path $engramHome 'root-lifecycle-read.json'
    if ($request.operation -eq "named_root_read" -and (Test-Path -LiteralPath $rootRead)) {
        Write-Result ([IO.File]::ReadAllText($rootRead) | ConvertFrom-Json)
        continue
    }
    if ($request.operation -eq "work_session_read") {
        # Framing-only transport response, not an identity or capability proof.
        Write-Result @{ binding = $null }
        continue
    }
    Write-ControlError "invalid_request" "unsupported fixture operation"
}

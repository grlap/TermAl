param([Parameter(Mandatory=$true)][string]$RequestPath,
  [Parameter(Mandatory=$true)][string]$CompilerScratch)
$ErrorActionPreference = 'Stop'
function Write-BootstrapPhase([string]$Phase) {
  [IO.File]::AppendAllText(($RequestPath + '.bootstrap.jsonl'), ((@{phase=$Phase;pid=$PID;compilerScratch=$CompilerScratch} | ConvertTo-Json -Compress) + [Environment]::NewLine))
}
Write-BootstrapPhase 'script-entered'
$request = Get-Content -LiteralPath $RequestPath -Raw -Encoding UTF8 | ConvertFrom-Json
Write-BootstrapPhase 'request-read'
try {
  # The guardian owns a short unique repository-local compiler directory and
  # removes it after this shell exits. The command keeps its original environment.
  $ownerOldTemp = $env:TEMP
  $ownerOldTmp = $env:TMP
  try {
    $env:TEMP = $CompilerScratch
    $env:TMP = $CompilerScratch
    Write-BootstrapPhase 'before-add-type'
    Add-Type -Path (Join-Path $PSScriptRoot 'test-process-owner-windows.cs') -ReferencedAssemblies 'System.dll','System.Core.dll','System.Web.Extensions.dll'
    Write-BootstrapPhase 'after-add-type'
  } finally {
    $env:TEMP = $ownerOldTemp
    $env:TMP = $ownerOldTmp
  }
  [TestCommandOwner]::Run([string]$request.command, [string[]]$request.args,
    [string]$request.cwd, [string]$request.log, [string]$request.ledger,
    [string]$request.nonce, [int]$request.cleanupBudgetMs, ($RequestPath + '.bootstrap.jsonl'))
  Write-BootstrapPhase 'native-returned'
} catch {
  Write-BootstrapPhase 'script-failed'
  [Console]::Error.WriteLine($_.Exception.ToString())
  exit 1
}

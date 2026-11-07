[CmdletBinding()]
param(
    [switch]$Offline
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if ($null -eq (Get-Command -Name lychee -CommandType Application -ErrorAction Ignore)) {
    throw 'lychee is required. Run scripts/Install-DevTools.ps1, then run this script again.'
}

$linkArguments = @(
    '--config', 'lychee.toml',
    '--files-from', (Join-Path -Path $PSScriptRoot -ChildPath 'lychee-inputs.txt')
)
if ($Offline) {
    $linkArguments += '--offline'
}

Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
try {
    & lychee @linkArguments
    $linkExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

exit $linkExitCode

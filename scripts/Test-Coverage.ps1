[CmdletBinding()]
param(
    [string]$OutputDirectory = [IO.Path]::Combine($PSScriptRoot, '..', 'target', 'coverage')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$OutputDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
try {
    & cargo install --locked cargo-llvm-cov
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    & rustup component add llvm-tools-preview
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    $reportArguments = @('--release', '--ignore-filename-regex', '[/\\](fixtures|test_fixtures)[/\\]')
    & cargo llvm-cov @reportArguments --locked --workspace --lib --bins --html --output-dir $OutputDirectory
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    $coverageReportJson = & cargo llvm-cov report @reportArguments --json --summary-only
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    $lineCoverage = ($coverageReportJson | ConvertFrom-Json).data[0].totals.lines
    if ($lineCoverage.count -le 0 -or $lineCoverage.covered -lt 0 -or $lineCoverage.covered -gt $lineCoverage.count) {
        throw 'The LLVM coverage report contains invalid or empty aggregate line counts.'
    }

    $coveragePercent = 100.0 * $lineCoverage.covered / $lineCoverage.count
    $coverageColor = if ($coveragePercent -ge 90) { 'brightgreen' } elseif ($coveragePercent -ge 75) { 'yellow' } else { 'red' }
    $badge = [ordered]@{
        schemaVersion = 1
        label = 'coverage'
        message = $coveragePercent.ToString('0.0', [Globalization.CultureInfo]::InvariantCulture) + '%'
        color = $coverageColor
    }
    $badgePath = [IO.Path]::Combine($OutputDirectory, 'html', 'coverage.json')
    [IO.File]::WriteAllText($badgePath, ($badge | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    Write-Host "Line coverage: $($badge.message)"
    Write-Host "Report: $([IO.Path]::Combine($OutputDirectory, 'html', 'index.html'))"
} finally {
    Pop-Location
}

[CmdletBinding()]
param(
    [string]$OutputDirectory = [IO.Path]::Combine($PSScriptRoot, '..', 'target', 'coverage')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if ($null -eq (Get-Command -Name cargo-llvm-cov -CommandType Application -ErrorAction Ignore)) {
    throw 'cargo-llvm-cov is required. Run scripts/Install-DevTools.ps1, then run this script again.'
}
if ($null -eq (Get-Command -Name rustup -CommandType Application -ErrorAction Ignore)) {
    throw 'Rustup is required to check coverage tools. Install Rust with rustup, then run scripts/Install-DevTools.ps1.'
}

$OutputDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
Push-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
try {
    $installedComponents = & rustup component list --installed
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if (-not ($installedComponents -match '^llvm-tools(?:-|$)')) {
        throw 'llvm-tools-preview is required. Run scripts/Install-DevTools.ps1, then run this script again.'
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

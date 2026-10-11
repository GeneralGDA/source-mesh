[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if ($null -eq (Get-Command -Name cargo -CommandType Application -ErrorAction Ignore)) {
    throw 'Cargo is required to run tests. Install Rust with rustup, then run this script again.'
}

Push-Location -LiteralPath $PSScriptRoot
try {
    Write-Host 'Running all workspace tests in release mode, including ignored tests that run rust-analyzer...'
    Write-Host 'Indexing test projects with rust-analyzer may take several minutes without output. Please wait.'
    & cargo test --release --locked --workspace --no-fail-fast -- --include-ignored --test-threads=1
    $testExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

exit $testExitCode

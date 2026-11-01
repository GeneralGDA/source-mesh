[CmdletBinding()]
param(
    [ValidateNotNullOrEmpty()]
    [string[]]$File = @(),

    [ValidateNotNullOrEmpty()]
    [string]$MutantPattern,

    [switch]$List,

    [ValidateRange(1, 2147483647)]
    [int]$Jobs = 1,

    [ValidateRange(1, 2147483647)]
    [int]$TestTimeoutSeconds = 120,

    [ValidateRange(1, 2147483647)]
    [int]$BuildTimeoutSeconds = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if ($null -eq (Get-Command -Name cargo -CommandType Application -ErrorAction Ignore)) {
    throw 'Cargo is required to run mutation tests. Install Rust with rustup, then run this script again.'
}
if ($null -eq (Get-Command -Name cargo-mutants -CommandType Application -ErrorAction Ignore)) {
    throw 'cargo-mutants is required. Run scripts/Install-DevTools.ps1, then run this script again.'
}

$repositoryDirectory = Split-Path -Parent $PSScriptRoot
$mutationOutputDirectory = Join-Path -Path (Join-Path -Path $repositoryDirectory -ChildPath 'work') -ChildPath 'mutants'
$mutationArguments = @(
    '--package', 'source-mesh-core',
    '--test-package', 'source-mesh-core',
    '--cargo-arg=--locked',
    '--cargo-arg=--lib',
    '--jobs', $Jobs.ToString(),
    '--timeout', $TestTimeoutSeconds.ToString(),
    '--build-timeout', $BuildTimeoutSeconds.ToString(),
    '--output', $mutationOutputDirectory
)
foreach ($filePattern in $File) {
    $mutationArguments += @('--file', $filePattern)
}
if ($PSBoundParameters.ContainsKey('MutantPattern')) {
    $mutationArguments += @('--re', $MutantPattern)
}
if ($List) {
    $mutationArguments += '--list'
}

Push-Location -LiteralPath $repositoryDirectory
try {
    & cargo mutants @mutationArguments
    $mutationExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

exit $mutationExitCode

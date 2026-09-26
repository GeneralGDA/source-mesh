[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($null -eq (Get-Command -Name cargo -ErrorAction Ignore)) {
    throw 'Cargo is required to install agent tools. Install Rust with rustup, then run this script again.'
}

$commandPackages = [ordered]@{
    rg         = 'ripgrep'
    fd         = 'fd-find'
    jaq        = 'jaq'
    'ast-grep' = 'ast-grep'
}

foreach ($commandName in $commandPackages.Keys) {
    $installedCommand = Get-Command -Name $commandName -CommandType Application -ErrorAction Ignore |
        Select-Object -First 1
    if ($null -ne $installedCommand) {
        Write-Host "$commandName is already available at $($installedCommand.Source)."
        continue
    }

    $packageName = $commandPackages[$commandName]
    Write-Host "Installing $commandName from Cargo package $packageName..."
    & cargo install --locked $packageName
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo failed to install $packageName for $commandName with exit code $LASTEXITCODE."
    }

    $installedCommand = Get-Command -Name $commandName -CommandType Application -ErrorAction Ignore |
        Select-Object -First 1
    if ($null -eq $installedCommand) {
        throw "$commandName was installed but is not available on PATH. Restart PowerShell and run this script again."
    }
}

Write-Host 'All agent tools are available.'

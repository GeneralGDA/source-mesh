[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$SummaryPath,

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string[]]$ReportPaths
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$reports = foreach ($reportPath in $ReportPaths) {
    try {
        $report = Get-Content -LiteralPath $reportPath -Raw -Encoding utf8
        if ([string]::IsNullOrWhiteSpace($report)) {
            "Report is empty: ``$reportPath``. See the dependency cycle check log."
        } else {
            $report
        }
    } catch [System.Management.Automation.ItemNotFoundException] {
        "Report not available: ``$reportPath``. See the dependency cycle check log."
    }
}

$summary = $reports -join "`n`n"
if ([Text.Encoding]::UTF8.GetByteCount($summary) -gt 1MB) {
    $summary = "Dependency cycle reports exceed GitHub's 1 MiB summary limit.`n`n" +
        'See the `source-mesh-cycle-diagnostics` artifact for full reports.'
}

Set-Content -LiteralPath $SummaryPath -Value $summary -Encoding utf8NoBOM -NoNewline

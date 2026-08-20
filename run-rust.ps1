param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$AppArguments
)

$ErrorActionPreference = "Stop"
$appDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$cargo = Get-Command cargo.exe -ErrorAction Stop

Push-Location $appDirectory
try {
    Write-Host "Building the Rust transcriber..."
    & $cargo.Source build --release
    if ($LASTEXITCODE -ne 0) {
        throw "The Rust transcriber could not be built."
    }

    $executable = Join-Path $appDirectory (
        "target\release\vatsim-live-transcriber.exe"
    )
    & $executable @AppArguments
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}

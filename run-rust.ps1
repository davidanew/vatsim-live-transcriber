param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$AppArguments
)

$ErrorActionPreference = "Stop"
$appDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$cargo = Get-Command cargo.exe -ErrorAction Stop
$distDirectory = Join-Path $appDirectory (
    "dist\vatsim-live-transcriber-rust-0.1.0-windows-x64"
)

Push-Location $appDirectory
try {
    Write-Host "Building the Rust transcriber..."
    & $cargo.Source build --release
    if ($LASTEXITCODE -ne 0) {
        throw "The Rust transcriber could not be built."
    }

    $builtExecutable = Join-Path $appDirectory (
        "target\release\vatsim-live-transcriber.exe"
    )
    New-Item -ItemType Directory -Path $distDirectory -Force | Out-Null
    $distExecutable = Join-Path $distDirectory "vatsim-live-transcriber.exe"

    Write-Host "Updating the dist executable..."
    Copy-Item -LiteralPath $builtExecutable -Destination $distExecutable -Force
    Copy-Item -LiteralPath (Join-Path $appDirectory "keywords.txt") `
        -Destination $distDirectory -Force

    # Launch the exact file that will be tested and committed.
    & $distExecutable @AppArguments
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$AppArguments
)

$ErrorActionPreference = "Stop"
$appDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$venvDirectory = Join-Path $appDirectory ".venv"
$python = Join-Path $venvDirectory "Scripts\python.exe"

function Find-Uv {
    $command = Get-Command uv.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $candidates = @(
        (Join-Path $env:USERPROFILE ".local\bin\uv.exe"),
        (Join-Path $env:USERPROFILE ".cargo\bin\uv.exe")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    return $null
}

$uv = Find-Uv
if (-not $uv) {
    Write-Host "The uv Python manager is required for first-time setup."
    $answer = Read-Host "Install uv from astral.sh now? [Y/n]"
    if ($answer -and $answer -notmatch "^[Yy]") {
        throw "Setup cancelled. Install uv from https://docs.astral.sh/uv/ and rerun this script."
    }

    Invoke-RestMethod "https://astral.sh/uv/install.ps1" | Invoke-Expression
    $uv = Find-Uv
    if (-not $uv) {
        throw "uv was installed but could not be found. Open a new PowerShell window and rerun this script."
    }
}

if (-not (Test-Path -LiteralPath $python -PathType Leaf)) {
    Write-Host "Creating the private Python environment..."
    & $uv venv --python 3.11 $venvDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "Could not create the Python environment."
    }
}

Write-Host "Checking program dependencies..."
& $uv pip install --quiet --python $python --requirements (Join-Path $appDirectory "requirements.txt")
if ($LASTEXITCODE -ne 0) {
    throw "Could not install the program dependencies."
}

& $python (Join-Path $appDirectory "app.py") @AppArguments
exit $LASTEXITCODE

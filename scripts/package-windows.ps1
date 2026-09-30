[CmdletBinding()]
param(
    [string]$Version
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $package = $metadata.packages | Where-Object { $_.name -eq "lumix-33-lut-maker" } | Select-Object -First 1
    if (-not $package) {
        throw "Cargo metadata does not contain lumix-33-lut-maker."
    }
    if (-not $Version) {
        $Version = $package.version
    }
    if ($Version -ne $package.version) {
        throw "Requested version $Version does not match Cargo version $($package.version)."
    }

    $sourceExe = Join-Path $projectRoot "target\x86_64-pc-windows-msvc\release\lumix-33-lut-maker.exe"
    if (-not (Test-Path -LiteralPath $sourceExe -PathType Leaf)) {
        throw "Release executable not found at $sourceExe. Run cargo build --release --target x86_64-pc-windows-msvc first."
    }

    $distDir = Join-Path $projectRoot "dist\$Version"
    $outputExe = Join-Path $distDir "LUMIX-LUT-Maker-$Version-windows-x64.exe"
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null
    if (Test-Path -LiteralPath $outputExe) {
        Remove-Item -LiteralPath $outputExe -Force
    }
    Copy-Item -LiteralPath $sourceExe -Destination $outputExe
    if ((Get-Item -LiteralPath $outputExe).Length -le 0) {
        throw "Packaged executable is empty."
    }
    Write-Output $outputExe
}
finally {
    Pop-Location
}

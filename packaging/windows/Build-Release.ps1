#Requires -Version 7.3
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, 2147483647)][int]$Build,
    [string]$WinSparkle = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseTools\WinSparkle-0.9.4\WinSparkle-0.9.4'),
    [string]$InnoCompiler = (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    [string]$KeyDirectory = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseKeys'),
    [ValidatePattern('^[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*$')][string]$VersionSuffix,
    [string]$OutputDirectory,
    [switch]$BuildOnly
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$target = 'x86_64-pc-windows-msvc'
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$InnoCompiler = (Resolve-Path -LiteralPath $InnoCompiler).Path
$WinSparkle = (Resolve-Path -LiteralPath $WinSparkle).Path
$sparkleDll = Join-Path $WinSparkle 'x64\Release\WinSparkle.dll'

if ((Get-FileHash -LiteralPath $sparkleDll -Algorithm SHA256).Hash -ne '9B43B1C16EE39FB9A91B5BD75138767898779510E0836BE2919250607CDBE8AB') {
    throw 'WinSparkle runtime checksum mismatch. Use the pinned WinSparkle release.'
}
$publicKey = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'update-public-key.txt') -Raw).Trim()
if ([Convert]::FromBase64String($publicKey).Length -ne 32) {
    throw 'The update public key must be a base64-encoded 32-byte Ed25519 public key.'
}
$envSettings = @{
    BALANCE_UPDATE_BUILD = [string]$Build
    BALANCE_UPDATE_PUBLIC_KEY = $publicKey
    CARGO_PROFILE_RELEASE_STRIP = 'symbols'
    CARGO_ENCODED_RUSTFLAGS = @('-C', 'target-feature=+crt-static', "--remap-path-prefix=$root=.") -join [char]31
}
$previousSettings = @{}
Push-Location -LiteralPath $root
try {
    if (git status --porcelain) {
        throw 'Build releases from a clean worktree so artifacts match their source commit.'
    }
    $commit = (git rev-parse HEAD).Trim()
    $manifest = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    $version = ($manifest.packages | Where-Object name -eq 'balance').version
    if ($version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') {
        throw 'Cargo.toml must contain a release version that can be used in asset names.'
    }

    if ($VersionSuffix) {
        $separator = if ($version.Contains('-')) { '.' } else { '-' }
        $version += "$separator$VersionSuffix"
    }
    $envSettings.BALANCE_UPDATE_VERSION = $version
    $output = if ($OutputDirectory) {
        [IO.Path]::GetFullPath($OutputDirectory)
    } else {
        Join-Path $root ('target\windows-release\{0}-{1}-{2}' -f $version, $Build, [guid]::NewGuid().ToString('N').Substring(0, 8))
    }
    if ((Test-Path -LiteralPath $output) -and (Get-ChildItem -LiteralPath $output -Force)) {
        throw 'Choose a new or empty release output directory.'
    }
    $payload = Join-Path $output 'payload'
    New-Item -ItemType Directory -Path $payload | Out-Null

    foreach ($name in $envSettings.Keys) {
        $previousSettings[$name] = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, $envSettings[$name])
    }
    $targetDirectory = Join-Path $root 'target'
    cargo build --locked --release --target $target --target-dir $targetDirectory --features windows-updater
    Copy-Item -LiteralPath (Join-Path $targetDirectory "$target\release\balance.exe"), $sparkleDll, (Join-Path $root 'LICENSE') -Destination $payload

    python (Join-Path $PSScriptRoot 'collect-notices.py') $payload
    $sparkleNotices = Join-Path $payload 'licenses\WinSparkle-0.9.4'
    New-Item -ItemType Directory -Path $sparkleNotices | Out-Null
    foreach ($notice in @('AUTHORS', 'COPYING', 'COPYING.expat')) {
        Copy-Item -LiteralPath (Join-Path $WinSparkle $notice) -Destination $sparkleNotices
    }

    & $InnoCompiler "/DVersion=$version" "/DBuild=$Build" "/DPackageDir=$payload" "/O$output" (Join-Path $PSScriptRoot 'Balance.iss')
    $installerName = "balance-$version-windows-x64-setup.exe"
    $installer = Join-Path $output $installerName
    $sha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    [ordered]@{
        version = $version
        build = $Build
        tag = "v$version"
        commit = $commit
        installer = $installerName
        length = (Get-Item -LiteralPath $installer).Length
        sha256 = $sha256
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'release.json') -Encoding utf8
    if (-not $BuildOnly) {
        & (Join-Path $PSScriptRoot 'Sign-Release.ps1') -BuildDirectory $output -WinSparkle $WinSparkle -KeyDirectory $KeyDirectory
    }
    Write-Output $output
} finally {
    foreach ($name in $previousSettings.Keys) {
        [Environment]::SetEnvironmentVariable($name, $previousSettings[$name])
    }
    Pop-Location
}

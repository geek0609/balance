#Requires -Version 7.3
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, 2147483647)][int]$Build,
    [string]$WinSparkle = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseTools\WinSparkle-0.9.4\WinSparkle-0.9.4'),
    [string]$InnoCompiler = (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    [string]$KeyDirectory = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseKeys')
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$target = 'x86_64-pc-windows-msvc'
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$InnoCompiler = (Resolve-Path -LiteralPath $InnoCompiler).Path
$WinSparkle = (Resolve-Path -LiteralPath $WinSparkle).Path
$KeyDirectory = (Resolve-Path -LiteralPath $KeyDirectory).Path
$sparkleDll = Join-Path $WinSparkle 'x64\Release\WinSparkle.dll'
$sparkleTool = Join-Path $WinSparkle 'bin\winsparkle-tool.exe'
$protectedKey = Join-Path $KeyDirectory 'windows-x64.dpapi'

foreach ($component in @(
    @{ Path = $sparkleDll; Hash = '9B43B1C16EE39FB9A91B5BD75138767898779510E0836BE2919250607CDBE8AB' },
    @{ Path = $sparkleTool; Hash = '0C5412C481BE6146313A2AF46CCE4A928913C94754763A75BB11EF15C9097CC3' }
)) {
    if ((Get-FileHash -LiteralPath $component.Path -Algorithm SHA256).Hash -ne $component.Hash) {
        throw "Checksum mismatch for $($component.Path). Use the pinned WinSparkle release."
    }
}
$publicKey = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'update-public-key.txt') -Raw).Trim()
if ([Convert]::FromBase64String($publicKey).Length -ne 32) {
    throw 'The update public key must be a base64-encoded 32-byte Ed25519 public key.'
}
if (-not (Test-Path -LiteralPath $protectedKey -PathType Leaf)) {
    throw 'The protected update signing key is missing.'
}
if (-not (Get-Acl -LiteralPath $KeyDirectory).AreAccessRulesProtected) {
    throw 'Use the restricted signing-key directory with inherited access disabled.'
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

    $output = Join-Path $root ('target\windows-release\{0}-{1}-{2}' -f $version, $Build, [guid]::NewGuid().ToString('N').Substring(0, 8))
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
    $temporaryKey = Join-Path $KeyDirectory ([guid]::NewGuid().ToString('N') + '.key')
    $rawKey = [System.Security.Cryptography.ProtectedData]::Unprotect(
        [IO.File]::ReadAllBytes($protectedKey), $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser
    )
    try {
        [IO.File]::WriteAllBytes($temporaryKey, $rawKey)
        $signature = ((& $sparkleTool sign --private-key-file $temporaryKey $installer) -join '').Trim()
    } finally {
        [Array]::Clear($rawKey, 0, $rawKey.Length)
        if (Test-Path -LiteralPath $temporaryKey) { Remove-Item -LiteralPath $temporaryKey }
    }
    if ([Convert]::FromBase64String($signature).Length -ne 64) {
        throw 'The signing tool did not produce a valid Ed25519 signature.'
    }
    & $sparkleTool verify --public-key $publicKey --signature $signature $installer

    $sha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath ($installer + '.sha256') -Value "$sha256  $installerName" -Encoding ascii
    [ordered]@{
        version = $version
        build = $Build
        tag = "v$version"
        commit = $commit
        installer = $installerName
        length = (Get-Item -LiteralPath $installer).Length
        sha256 = $sha256
        signature = $signature
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'release.json') -Encoding utf8
    Write-Output $output
} finally {
    foreach ($name in $previousSettings.Keys) {
        [Environment]::SetEnvironmentVariable($name, $previousSettings[$name])
    }
    Pop-Location
}

#Requires -Version 7.3
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BuildDirectory,
    [string]$WinSparkle = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseTools\WinSparkle-0.9.4\WinSparkle-0.9.4'),
    [string]$KeyDirectory = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseKeys')
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$BuildDirectory = (Resolve-Path -LiteralPath $BuildDirectory).Path
$metadataPath = Join-Path $BuildDirectory 'release.json'
$release = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json -AsHashtable
if (
    $release.version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$' -or
    $release.installer -cne "balance-$($release.version)-windows-x64-setup.exe" -or
    $release.commit -cnotmatch '^[0-9a-f]{40}$' -or
    ($env:GITHUB_ACTIONS -eq 'true' -and $release.commit -cne $env:GITHUB_SHA)
) {
    throw 'The installer metadata does not match the release build.'
}
$installer = Join-Path $BuildDirectory $release.installer
$sha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
if ($sha256 -cne $release.sha256 -or (Get-Item -LiteralPath $installer).Length -ne $release.length) {
    throw 'The installer differs from the recorded build.'
}
$sparkleTool = Join-Path (Resolve-Path -LiteralPath $WinSparkle).Path 'bin\winsparkle-tool.exe'
if ((Get-FileHash -LiteralPath $sparkleTool -Algorithm SHA256).Hash -ne '0C5412C481BE6146313A2AF46CCE4A928913C94754763A75BB11EF15C9097CC3') {
    throw 'The pinned WinSparkle signing tool is required.'
}
$publicKey = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'update-public-key.txt') -Raw).Trim()

if ($env:GITHUB_ACTIONS -eq 'true') {
    if (-not $env:BALANCE_UPDATE_SIGNING_KEY) {
        throw 'The Windows release signing secret is unavailable.'
    }
    $keyFolder = $env:RUNNER_TEMP
    $rawKey = [Convert]::FromBase64String($env:BALANCE_UPDATE_SIGNING_KEY)
    $env:BALANCE_UPDATE_SIGNING_KEY = $null
} else {
    $keyFolder = (Resolve-Path -LiteralPath $KeyDirectory).Path
    $protectedKey = Join-Path $keyFolder 'windows-x64.dpapi'
    if (-not (Get-Acl -LiteralPath $keyFolder).AreAccessRulesProtected) {
        throw 'Use the restricted signing-key directory with inherited access disabled.'
    }
    $rawKey = [System.Security.Cryptography.ProtectedData]::Unprotect(
        [IO.File]::ReadAllBytes($protectedKey), $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser
    )
}

$temporaryKey = Join-Path $keyFolder ('balance-signing-' + [guid]::NewGuid().ToString('N') + '.key')
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
Set-Content -LiteralPath ($installer + '.sha256') -Value "$sha256  $($release.installer)" -Encoding ascii
$release.signature = $signature
$release | ConvertTo-Json | Set-Content -LiteralPath $metadataPath -Encoding utf8

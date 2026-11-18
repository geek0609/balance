#Requires -Version 7.3
[CmdletBinding()]
param([switch]$SignerOnly)

$ErrorActionPreference = 'Stop'
if (-not $env:RUNNER_TEMP) {
    throw 'Use this bootstrap on a GitHub hosted Windows runner.'
}
$tools = Join-Path $env:RUNNER_TEMP 'balance-tools'
New-Item -ItemType Directory -Force -Path $tools | Out-Null

function Receive-VerifiedFile($Url, $Name, $Sha256) {
    $file = Join-Path $tools $Name
    Invoke-WebRequest -Uri $Url -OutFile $file -MaximumRetryCount 3 -RetryIntervalSec 2
    if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $Sha256) {
        throw "$Name failed SHA-256 verification."
    }
    return $file
}

$zip = Receive-VerifiedFile -Url 'https://github.com/vslavik/winsparkle/releases/download/v0.9.4/WinSparkle-0.9.4.zip' -Name 'WinSparkle-0.9.4.zip' -Sha256 '6037df37fc263bd1650a1c4949681a9d40ffe991d01f35892a406cb5d103c976'
Expand-Archive -LiteralPath $zip -DestinationPath $tools
$paths = @{ WinSparkle = Join-Path $tools 'WinSparkle-0.9.4' }

if (-not $SignerOnly) {
    $setup = Receive-VerifiedFile -Url 'https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-6.7.3.exe' -Name 'innosetup-6.7.3.exe' -Sha256 '9c73c3bae7ed48d44112a0f48e66742c00090bdb5bef71d9d3c056c66e97b732'
    $directory = Join-Path $tools 'InnoSetup-6.7.3'
    $process = Start-Process -FilePath $setup -ArgumentList @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', ('/DIR="' + $directory + '"')
    ) -Wait -PassThru -WindowStyle Hidden
    $paths.InnoCompiler = Join-Path $directory 'ISCC.exe'
    if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $paths.InnoCompiler -PathType Leaf)) {
        throw "Inno Setup bootstrap failed with exit code $($process.ExitCode)."
    }
}
return $paths

#Requires -Version 7.3
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BuildDirectory,
    [Parameter(Mandatory)][string]$NotesFile,
    [string]$WinSparkle = (Join-Path $env:LOCALAPPDATA 'BalanceReleaseTools\WinSparkle-0.9.4\WinSparkle-0.9.4')
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$repo = 'geek0609/balance'
$remote = "https://github.com/$repo.git"
$feedFile = 'windows-x64.xml'
$feedRef = 'refs/heads/updates'
$ns = 'http://www.andymatuschak.org/xml-namespaces/sparkle'
$BuildDirectory = (Resolve-Path -LiteralPath $BuildDirectory).Path
$NotesFile = (Resolve-Path -LiteralPath $NotesFile).Path
$metadata = Get-Content -LiteralPath (Join-Path $BuildDirectory 'release.json') -Raw | ConvertFrom-Json

if (
    $metadata.version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$' -or
    $metadata.tag -cne "v$($metadata.version)" -or
    $metadata.commit -cnotmatch '^[0-9a-f]{40}$' -or
    $metadata.build -lt 1 -or $metadata.build -gt [int]::MaxValue -or
    $metadata.installer -cne "balance-$($metadata.version)-windows-x64-setup.exe" -or
    [Convert]::FromBase64String($metadata.signature).Length -ne 64
) {
    throw 'Invalid release metadata.'
}

$installer = Join-Path $BuildDirectory $metadata.installer
$checksum = $installer + '.sha256'
$digest = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
if (
    $digest -cne $metadata.sha256 -or
    (Get-Item -LiteralPath $installer).Length -ne $metadata.length -or
    (Get-Content -LiteralPath $checksum -Raw).Trim() -cne "$digest  $($metadata.installer)"
) {
    throw 'The installer or checksum does not match its build metadata.'
}

$publicKey = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'update-public-key.txt') -Raw).Trim()
$signer = Join-Path $WinSparkle 'bin\winsparkle-tool.exe'
if ((Get-FileHash -LiteralPath $signer -Algorithm SHA256).Hash -ne '0C5412C481BE6146313A2AF46CCE4A928913C94754763A75BB11EF15C9097CC3') {
    throw 'The pinned WinSparkle signing tool is required to verify the release.'
}
& $signer verify --public-key $publicKey --signature $metadata.signature $installer

function Get-Release {
    $filter = '.[] | select(.tag_name == "' + $metadata.tag + '")'
    gh api "repos/$repo/releases?per_page=100" --paginate --jq $filter | ConvertFrom-Json
}

$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
Push-Location -LiteralPath $root
try {
    git fetch --no-tags $remote refs/heads/main
    git merge-base --is-ancestor $metadata.commit FETCH_HEAD

    $currentFeed = git ls-remote $remote $feedRef
    $parentCommit = $null
    $alreadyPublished = $false
    if ($currentFeed) {
        git fetch --no-tags $remote $feedRef
        $parentCommit = (git rev-parse FETCH_HEAD).Trim()
        [xml]$previousFeed = (git show ('{0}:{1}' -f $parentCommit, $feedFile)) -join [Environment]::NewLine
        $latest = $previousFeed.rss.channel.item
        $latestBuild = [int]$latest.GetElementsByTagName('version', $ns).Item(0).InnerText
        if ($metadata.build -le $latestBuild) {
            $previousSignature = $latest.enclosure.GetAttribute('edSignature', $ns)
            $previousVersion = $latest.GetElementsByTagName('shortVersionString', $ns).Item(0).InnerText
            if ($metadata.build -eq $latestBuild -and $metadata.version -ceq $previousVersion -and $metadata.signature -ceq $previousSignature) {
                $alreadyPublished = $true
            } else {
                throw "Choose a build number above the published build $latestBuild."
            }
        }
    }

    $tag = $metadata.tag
    $prerelease = $metadata.version.Contains('-')
    $release = Get-Release
    if (-not $release) {
        $create = @($tag, '--repo', $repo, '--draft', '--target', $metadata.commit, '--title', "Balance $($metadata.version)", '--notes-file', $NotesFile)
        if ($prerelease) { $create += '--prerelease' }
        gh release create @create
        $release = Get-Release
    }
    if (-not $release -or $release.prerelease -ne $prerelease) {
        throw 'GitHub returned an unexpected release.'
    }
    # GitHub returns the requested target for a tag created by the release. For
    # any pre-existing tag, resolve the actual commit before using the release.
    $tagRefs = @(git ls-remote $remote "refs/tags/$tag" "refs/tags/$tag^{}")
    $target = if ($tagRefs) { ($tagRefs[-1] -split '\s+')[0] } else { $release.target_commitish }
    if ($target -cne $metadata.commit) {
        throw 'The release points to a different source revision.'
    }

    foreach ($file in @($installer, $checksum)) {
        $filename = Split-Path -Leaf $file
        $asset = $release.assets | Where-Object name -ceq $filename
        if (-not $asset -and $release.draft) {
            gh release upload $tag $file --repo $repo
            $release = Get-Release
            $asset = $release.assets | Where-Object name -ceq $filename
        }
        $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($asset.state -cne 'uploaded' -or $asset.digest -cne "sha256:$hash") {
            throw "GitHub returned a missing or mismatched asset for $filename."
        }
    }

    if ($release.draft) {
        gh release edit $tag --repo $repo --draft=false
    }
    $release = Get-Release
    if (-not $release.published_at -or $release.draft) {
        throw 'The release must be public before updating the feed.'
    }
    $publishedRefs = @(git ls-remote $remote "refs/tags/$tag" "refs/tags/$tag^{}")
    if (-not $publishedRefs -or ($publishedRefs[-1] -split '\s+')[0] -cne $metadata.commit) {
        throw 'The public release tag points to a different source revision.'
    }
    if ($alreadyPublished) {
        Write-Output "The update feed already lists $tag."
        return
    }

    $published = ([DateTimeOffset]$release.published_at).UtcDateTime.ToString('r', [Globalization.CultureInfo]::InvariantCulture)
    $assetUrl = "https://github.com/$repo/releases/download/$tag/$($metadata.installer)"
    $feed = @"
<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:sparkle="$ns">
  <channel>
    <title>Balance for Windows x64</title>
    <link>https://github.com/$repo/releases</link>
    <description>Updates for installed Windows copies of Balance</description>
    <item>
      <title>Balance $($metadata.version)</title>
      <pubDate>$published</pubDate>
      <sparkle:version>$($metadata.build)</sparkle:version>
      <sparkle:shortVersionString>$($metadata.version)</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>10.0</sparkle:minimumSystemVersion>
      <enclosure url="$assetUrl" length="$($metadata.length)" type="application/octet-stream"
        sparkle:os="windows-x64" sparkle:edSignature="$($metadata.signature)"
        sparkle:installerArguments="/SILENT /SP- /NORESTART" />
    </item>
  </channel>
</rss>
"@
    $localFeed = Join-Path $BuildDirectory $feedFile
    Set-Content -LiteralPath $localFeed -Value $feed -Encoding utf8
    $blob = (git hash-object -w -- $localFeed).Trim()
    $priorIndex = $env:GIT_INDEX_FILE
    $feedIndex = Join-Path $BuildDirectory ("feed-" + [guid]::NewGuid().ToString('N') + '.index')
    try {
        $env:GIT_INDEX_FILE = $feedIndex
        git update-index --add --cacheinfo "100644,$blob,$feedFile"
        $tree = (git write-tree).Trim()
    } finally {
        $env:GIT_INDEX_FILE = $priorIndex
        if (Test-Path -LiteralPath $feedIndex) { Remove-Item -LiteralPath $feedIndex }
    }
    $commitArgs = @($tree, '-m', "Publish Balance $($metadata.version) update metadata")
    if ($parentCommit) { $commitArgs += @('-p', $parentCommit) }
    $feedCommit = (git commit-tree @commitArgs).Trim()
    git push $remote ('{0}:{1}' -f $feedCommit, $feedRef)
    Write-Output $release.html_url
} finally {
    Pop-Location
}

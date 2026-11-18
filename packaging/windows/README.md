# Windows releases

The Windows x64 installer bundles Balance, the WinSparkle runtime and third-party notices. It installs under the current user and uses a registered Start menu shortcut. The database remains in Balance's local application-data directory across upgrades and uninstall.

Installed builds check the public Windows update feed on the repository's [updates branch](https://github.com/geek0609/balance/tree/updates) hourly while Balance runs. The installer enables automatic checks for a new user and preserves the user's choice on upgrade. After the user approves an update, WinSparkle verifies the installer's Ed25519 signature before running it. Setup reopens Balance when installation completes.

## Automatic releases

The [Windows updates workflow](https://github.com/geek0609/balance/actions/workflows/windows-updates.yml) publishes a signed Windows preview after a successful build from `main`. It appends `.ci.<run>.<attempt>` to the preview version and publishes the installer and SHA-256 file before advancing the update feed. When pushes queue up, the newest commit is eligible for publishing.

The build job has read-only repository access. A separate publish job uses the `windows-updates` environment, restricted to `main`, and reads the encrypted `BALANCE_UPDATE_SIGNING_KEY` secret only while signing. The secret contains the base64-encoded bytes of the WinSparkle private key matching [update-public-key.txt](update-public-key.txt). Local signing still uses the protected release workstation key.

## Local builds

Build from a clean checkout. Use PowerShell 7.3 or newer, Git, GitHub CLI, Python 3, Rust with the `x86_64-pc-windows-msvc` target and the Visual Studio C++ build tools, Inno Setup 6.7.3 and WinSparkle 0.9.4. Install the latter tools from their upstream releases.

- [Inno Setup 6.7.3](https://github.com/jrsoftware/issrc/releases/tag/is-6_7_3)
- [WinSparkle 0.9.4](https://github.com/vslavik/winsparkle/releases/tag/v0.9.4)

The scripts default to the per-user Inno Setup installation and the official WinSparkle binary ZIP extracted to `%LOCALAPPDATA%\BalanceReleaseTools\WinSparkle-0.9.4`. Set `-InnoCompiler` and `-WinSparkle` to select other paths. The WinSparkle ZIP has SHA-256 `6037df37fc263bd1650a1c4949681a9d40ffe991d01f35892a406cb5d103c976`.

Choose a positive build number for a local installer. This example rebuilds 0.1.0-alpha.2 from its source revision.

    pwsh -File packaging/windows/Build-Release.ps1 -Build 2

The script produces the installer, its SHA-256 file and signed release metadata under `target/windows-release`. The publisher can also take a manually signed build and a Markdown release notes file. A public release must use a unique version and a build number that advances the feed. The workflow is the default publisher for `main`.

    pwsh -File packaging/windows/Publish-Release.ps1 -BuildDirectory <output-directory> -NotesFile <notes.md>

Publishing creates a GitHub release for the build commit, checks the uploaded assets and then advances the update feed. Retry with the same build directory if publication was interrupted. Keep the installer and release metadata together.

## Update signing key

The release workstation has the matching private key in `%LOCALAPPDATA%\BalanceReleaseKeys\windows-x64.dpapi`. Windows Data Protection ties the key to its Windows account. The key directory has restricted access and is separate from the repository and installer. The matching public key is in [update-public-key.txt](update-public-key.txt).

The release process decrypts the key only for the signing tool and removes the temporary key file afterwards. Keep the release account and a secure recovery copy of its signing material available for future releases. Changing the public key requires an installer with the replacement key. Existing installations verify updates with the key already embedded in their binary.

The update signature is independent of Windows Authenticode. The preview installer does not currently carry an Authenticode signature.

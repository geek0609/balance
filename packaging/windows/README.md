# Windows releases

The Windows x64 installer bundles Balance, the WinSparkle runtime and third-party notices. It installs under the current user and uses a registered Start menu shortcut. The database remains in Balance's local application-data directory across upgrades and uninstall.

Installed builds are configured for the public Windows update feed on the repository's [updates branch](https://github.com/geek0609/balance/tree/updates). The installer enables automatic checks for a new user and preserves the user's choice on upgrade. WinSparkle verifies the Ed25519 signature of the installer before running it. A user approves the update and reopens Balance after setup completes.

## Build and publish

Build from a clean checkout. Use PowerShell 7.3 or newer, Git, GitHub CLI, Python 3, Rust with the `x86_64-pc-windows-msvc` target and the Visual Studio C++ build tools, Inno Setup 6.7.3 and WinSparkle 0.9.4. Install the latter tools from their upstream releases.

- [Inno Setup 6.7.3](https://github.com/jrsoftware/issrc/releases/tag/is-6_7_3)
- [WinSparkle 0.9.4](https://github.com/vslavik/winsparkle/releases/tag/v0.9.4)

The scripts default to the per-user Inno Setup installation and the official WinSparkle binary ZIP extracted to `%LOCALAPPDATA%\BalanceReleaseTools\WinSparkle-0.9.4`. Set `-InnoCompiler` and `-WinSparkle` to select other paths. The WinSparkle ZIP has SHA-256 `6037df37fc263bd1650a1c4949681a9d40ffe991d01f35892a406cb5d103c976`.

Set the public release version in `Cargo.toml`, refresh `Cargo.lock`, and push that commit to `main`. Choose a positive build number greater than the one in the update feed, including for prereleases. Build number 2 corresponds to 0.1.0-alpha.2.

    pwsh -File packaging/windows/Build-Release.ps1 -Build 2

The script produces the installer, its SHA-256 file and signed release metadata under `target/windows-release`. Pass the resulting directory and a Markdown release notes file to the publisher.

    pwsh -File packaging/windows/Publish-Release.ps1 -BuildDirectory <output-directory> -NotesFile <notes.md>

Publishing creates a GitHub release for the build commit, checks the uploaded assets and then advances the update feed. Retry with the same build directory if publication was interrupted. Keep the installer and release metadata together.

## Update signing key

The release workstation has the matching private key in `%LOCALAPPDATA%\BalanceReleaseKeys\windows-x64.dpapi`. Windows Data Protection ties the key to its Windows account. The key directory has restricted access and is separate from the repository and installer. The matching public key is in [update-public-key.txt](update-public-key.txt).

The release process decrypts the key only for the signing tool and removes the temporary key file afterwards. Keep the release account and a secure recovery copy of its signing material available for future releases. Changing the public key requires an installer with the replacement key. Existing installations verify updates with the key already embedded in their binary.

The update signature is independent of Windows Authenticode. The preview installer does not currently carry an Authenticode signature.

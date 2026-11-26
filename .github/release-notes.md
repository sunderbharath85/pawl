**Pawl {{VERSION}}**: named pets (dog, cat, bird or robot) that watch and drive your coding agents over ACP, across all of your machines. Website: https://sunderbharath85.github.io/pawl/

## Downloads

| Platform | Desktop app (includes the `pawl` hub and CLI) | CLI only |
|---|---|---|
| Windows x64 | `Pawl_{{VERSION}}_x64-setup.exe` (or the `.msi`) | `pawl-cli-{{VERSION}}-x86_64-pc-windows-msvc.zip` |
| macOS, Apple silicon | `Pawl_{{VERSION}}_aarch64.dmg` | `pawl-cli-{{VERSION}}-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `Pawl_{{VERSION}}_x64.dmg` | `pawl-cli-{{VERSION}}-x86_64-apple-darwin.tar.gz` |
| Linux x64 | `.deb`, `.rpm` or `.AppImage` | `pawl-cli-{{VERSION}}-x86_64-unknown-linux-gnu.tar.gz` |

`SHA256SUMS.txt` has a checksum for every file.

## Install

- **Windows:** run the setup `.exe`. The builds are not code-signed yet, so SmartScreen may warn: choose **More info**, then **Run anyway**.
- **macOS:** open the `.dmg` and drag Pawl to Applications. The app is not notarized yet: the first time, right-click Pawl and choose **Open**. If macOS says the app is damaged, run `xattr -dr com.apple.quarantine /Applications/Pawl.app` once.
- **Linux:** install the `.deb` (`sudo apt install ./Pawl_*.deb`) or `.rpm`, or make the `.AppImage` executable and run it. Needs glibc 2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40 or later).
- **CLI only:** unpack the archive, put `pawl` on your PATH and run `pawl serve`.

Start Pawl and it starts its hub for you. To reach your machines from anywhere, deploy your own relay (see `services/relay/README.md`) and enter its URL and access key in **Settings → Relay** on each machine.

The phone app is not part of the release yet.

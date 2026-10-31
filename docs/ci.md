# Automated Windows validation

[Windows build and tests](https://github.com/husodrn46/windows-stats/actions/workflows/windows-ci.yml) runs on code pushes to `main`, pull requests and manual dispatch. Changes limited to Markdown, documentation images and issue templates do not trigger a new build. It uses a GitHub-hosted `windows-2022` machine; the maintainer's computer can be offline.

The workflow installs pinned Rust 1.99.0 and builds the MSVC target. This adds compiler coverage; the published v0.1.0 release was built with the GNU target documented in the README. CI artifacts are unsigned validation builds, not published release assets.

## Required checks

- Build the release executable with the Cargo lockfile frozen.
- Verify embedded product name and version, so a silently skipped resource compiler is a failure.
- Run the Rust unit suite and require a nonempty passing result. Failed tests retain their exit status. The current source has 22 tests.
- Run the CLI version command and collect two JSON samples. Require CPU/core data and valid nonzero memory readings in a separate scratch settings home.
- Download the published v0.1.0 ZIP, verify its exact SHA-256 and companion files, then check its own CLI version and two samples separately from the new CI build.
- Build the optional .NET Framework 4.8 sensor helper. No PawnIO driver is installed or activated.

Successful builds upload the executable, helper, notices and licence files for seven days. Rust test logs are uploaded even when a subsequent step fails. Each artifact name includes the source commit. The workflow does not sign, install or publish the application and has only read access to repository contents.

## Scope and limitations

Passing CI does not establish Windows 10/11 taskbar placement, Explorer restart recovery, physical sensor readings, screenshot appearance, or winget installation and removal. These require separate functional testing. Two existing DirectWrite tests return early if a text engine cannot be created; a passed test count therefore does not by itself prove rendering works on every machine.

To reproduce CLI checks on Windows after a local build:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\ci-smoke.ps1 -Executable target\release\WindowsStats.exe -WorkDirectory .devtmp\ci-smoke
```

The script starts only its own short-lived CLI processes and restores the previous `WINDOWS_STATS_HOME` binding. It does not start or close the normal taskbar instance, run an installer, or change antivirus settings.

## Releases

[Release package](https://github.com/husodrn46/windows-stats/actions/workflows/release.yml) runs only when a `v*` tag is pushed. It checks that the tag, `Cargo.toml` and the sensor helper carry the same version and that `docs/release-notes/<version>.md` exists, builds and tests with the same pinned MSVC toolchain, packages with `scripts/build.ps1`, smoke-checks the packaged executable and attaches the ZIP to a **draft** release with its SHA-256. A maintainer reviews and publishes the draft. Release packages from 0.1.1 on are MSVC builds; 0.1.0 was a local GNU build.

To release: bump the version in `Cargo.toml`, `Cargo.lock` and `helpers/sensors/Program.cs`, add the release notes, merge, then `git tag vX.Y.Z && git push origin vX.Y.Z`. Release a new version after every merged security fix.

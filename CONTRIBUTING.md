# Contributing to Windows Stats

English and Turkish contributions are welcome. For a larger change, open an issue first so we can discuss the scope.

## Build and test

Use the pinned Rust toolchain and the Windows build instructions in [README.md](README.md#build) or [README.tr.md](README.tr.md). Run `cargo test --release` on Windows and describe any relevant manual checks. The optional sensor helper needs the .NET SDK; see the README.

The application depends on Win32, Explorer and hardware counters. Cross-compiling on Linux checks compilation but does not validate taskbar integration, UI behavior or real sensor readings. Test changes to these features on Windows 10/11.

## Pull requests

Keep changes focused and describe the problem, resulting behavior and validation. Add a regression test when it can exercise the failure meaningfully. Include translations in English and Turkish for new user-facing text. Keep existing copyright, MIT attribution to mgvs/winstats and third-party notices intact.

Do not commit `.toolchain`, `.devtmp`, `target`, `dist`, logs or personal settings. Use a separate `WINDOWS_STATS_HOME` when testing so your normal settings and running instance are preserved. Never include private information in screenshots, logs or sample data.

## Bugs and ideas

Use the issue templates. Include the application version, Windows version/build and reproducible steps. Review and redact `WindowsStats.log` before attaching it. For sensor issues, include the relevant hardware and helper/PawnIO setup without serial numbers.

## Documentation images

[docs/screenshots.md](docs/screenshots.md) describes the isolated capture workflow. Publish only images rendered by Windows Stats, after reviewing them for private details. Do not capture the desktop or other applications.

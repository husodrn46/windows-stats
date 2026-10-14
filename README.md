# Windows Stats

**English** · [Türkçe](README.tr.md)

A lightweight, privacy-friendly system monitor that lives in the Windows 10 / 11 taskbar.
Inspired by [Stats](https://github.com/exelban/stats) for macOS and built on
[winstats](https://github.com/mgvs/winstats) by Oleksandr Zhabotynskyi (MIT).

- **A strip inside the taskbar**: CPU (a bar per core, P-core / E-core split, history chart), GPU,
  RAM, disk, network, battery and temperature widgets. When Windows does not allow embedding, the
  strip floats over the taskbar in the same place.
- **Click a widget for a detail panel**: history charts, top processes, cores and clocks, memory
  modules, physical disks (SSD/HDD, temperature), drives, network adapters (Wi-Fi network, IP, MAC,
  link speed), battery, sensors.
- **Settings window** (right click > Settings…): widgets and their order, colours, theme
  (system / dark / light), text and chart sizes, update interval (0.5 / 1 / 2 / 5 s), position,
  start with Windows, threshold notifications, sensors, privacy.
- **Notifications**, e.g. "warn when the CPU temperature stays above 85 °C for 30 seconds". Values
  the device cannot measure never raise a notification.
- **Interface in English and Turkish** (plus 27 more languages from winstats, partly translated);
  the Windows display language is picked automatically.

## Install

No administrator rights needed; installs for your user account only.

1. Download `WindowsStats-<version>-x64.zip` from
   [Releases](https://github.com/husodrn46/windows-stats/releases) and unzip it.
2. Double-click `Kur.cmd` ("install"). The program is copied to
   `%LOCALAPPDATA%\Programs\Windows Stats`, added to the Start menu and, if you like, started with
   Windows.
3. **Uninstall:** Settings > Apps > Windows Stats > Uninstall. The taskbar is left as it was; you
   are asked whether to delete your settings as well.

You can also just run `WindowsStats.exe` without installing.

> The executable is not code-signed. Some antivirus products flag new unsigned programs by
> heuristics; the source is here, and you can build it yourself (see Build).

## Usage

- **Left click:** the widget's detail panel. **Right click:** quick settings menu (*Settings…* at
  the top).
- **Drag and drop:** reorder the widgets on the strip.
- **Double click:** Task Manager.
- On Windows 11 the strip never covers the task list buttons: if the chosen side has no room it
  moves to the other side, and if that is not enough either, the trailing widgets are hidden until
  there is space again.

## Sensors

| Value | Source | Admin rights / driver |
|---|---|---|
| Disk temperature, SSD/HDD | Windows storage queries | Not needed |
| GPU temperature, fan | Display driver (what Task Manager shows) | Not needed (dedicated GPUs) |
| GPU clock, voltage, extra values | LibreHardwareMonitor helper | Not needed |
| CPU temperature, power, fans, motherboard voltages | LibreHardwareMonitor + PawnIO driver | **Needed** |

Advanced sensors are off by default (**Settings > Sensors**). When switched on, a separate process,
`sensors\WindowsStats.Sensors.exe`, runs (about 80–100 MB of memory). Windows Stats **never installs
a driver**. For CPU temperatures, install PawnIO yourself from its [official site](https://pawnio.eu)
and switch on "Run the helper as administrator" (a UAC prompt at every start). Sensors a device does
not support show **"Not available on this device"** instead of a made-up value.

## Privacy and security

- **No** telemetry, analytics or automatic update checks. No network ports, no internet connection
  needed.
- The only optional network activity is the latency measurement in the network panel. It is off by
  default, and when switched on it sends ICMP echo requests only to the address you choose.
- Never asks for administrator rights (`asInvoker`); uses only per-user keys (`HKCU`).
- Measuring runs on its own thread, so the window embedded in the taskbar never waits for it and
  cannot make Explorer hang. When Explorer restarts, the strip reattaches by itself.
- Settings live in `%APPDATA%\WindowsStats\config.toml`, the log in `WindowsStats.log`.

## Performance

Measured over five minutes on an Intel Core 7 240H (16 logical cores): **0.05% of the machine's
CPU** (0.8% of one core) and about **16 MB** of private memory. CPU load matches Task Manager within
2 points and RAM within 0.1 points (`scripts\compare-taskmgr.ps1`). Details: [REPORT.md](REPORT.md).

## Build

A project-local toolchain that needs no administrator rights (`.toolchain`, not in git):

- Rust 1.99, `x86_64-pc-windows-gnu`: `rustup-init.exe -y --no-modify-path --default-host x86_64-pc-windows-gnu`
  with `RUSTUP_HOME` / `CARGO_HOME` pointing to `.toolchain\rustup` and `.toolchain\cargo`.
- [llvm-mingw](https://github.com/mstorsjo/llvm-mingw): only for `windres` (icon and manifest),
  unpacked to `.toolchain\llvm-mingw`.
- .NET SDK 8+ (only for the optional sensor helper).

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Test
```

Output: `target\release\WindowsStats.exe` and `dist\WindowsStats-<version>-x64.zip` (app, sensor
helper, licences, `Kur.cmd`).

## Command line

```
WindowsStats.exe                   start
WindowsStats.exe --settings        open the settings window
WindowsStats.exe --install         install for this user (--quiet: no questions)
WindowsStats.exe --uninstall       uninstall (--quiet: no questions, settings kept)
WindowsStats.exe --quit            close the running app cleanly
WindowsStats.exe --dump 10 1000    print 10 samples as JSON lines
WindowsStats.exe --taskbar-items   list the taskbar's items (diagnostics)
WindowsStats.exe --version
```

## Tests

```powershell
cargo test --release                                                   # unit tests
powershell -ExecutionPolicy Bypass -File scripts\compare-taskmgr.ps1   # compare with Windows' own counters
```

`tools\uitest` is a screen-capture and mouse helper for UI tests (not part of the release).

## Acknowledgements

Windows Stats stands on the work of others. Thank you:

- **[Oleksandr Zhabotynskyi](https://github.com/mgvs)** for **[winstats](https://github.com/mgvs/winstats)**
  (MIT). Windows Stats is a fork of it: the taskbar embedding, the collectors, the panels, the
  context menu and the translations all started there.
- **[Serhiy Mytrovtsiy](https://github.com/exelban)** for **[Stats](https://github.com/exelban/stats)**
  (MIT). Its clean menu-bar widgets and panels are the model for this project's look and behaviour
  (no code from it is used).
- **The [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor)
  contributors** (MPL-2.0) for the library behind the optional sensor helper, and
  **[Blacktempel](https://github.com/Blacktempel)** for DiskInfoToolkit, RAMSPDToolkit and
  BlackSharp.
- **[namazso](https://github.com/namazso)** for **[PawnIO](https://pawnio.eu)** and PawnIO.Modules,
  which make CPU temperatures readable without the old, vulnerable WinRing0 driver.
- **James F. Bellinger** for [HidSharp](https://www.zer7.com/software/hidsharp) (Apache-2.0).
- **Microsoft and the [windows-rs](https://github.com/microsoft/windows-rs) contributors** for the
  Rust bindings to the Windows API, and the **.NET Foundation** for the .NET libraries.
- **The [serde](https://serde.rs) and [toml](https://github.com/toml-rs/toml) maintainers**,
  **[winresource](https://github.com/BenjaminRi/winresource)**, **Martin Storsjö** for
  [llvm-mingw](https://github.com/mstorsjo/llvm-mingw), and **the Rust project**.

## Licence

MIT. Details and the third-party components used: [LICENSE](LICENSE),
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md), `licenses\`.

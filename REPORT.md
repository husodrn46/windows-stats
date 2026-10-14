# Windows Stats: Development Report (version 0.1.0)

**English** · [Türkçe](RAPOR.md)

Test machine: Windows 11 Home Single Language, **build 26300 (Insider)**, Intel Core 7 240H
(16 logical cores, hybrid P/E), Intel Graphics (integrated), 16 GB RAM, two NVMe SSDs,
1920×1080 at 100%, one monitor, a mini PC without a battery. Antivirus: Kaspersky Small Office Security.

## 1. Choice of architecture

| Project | Licence | Assessment | Use |
|---|---|---|---|
| mgvs/winstats | MIT | Rust, Win32/GDI; taskbar embedding, panels and 29 languages already there | **Base** (forked with its git history, credits kept) |
| exelban/stats | MIT | Swift/macOS; its code cannot be carried over to Windows | Design and behaviour inspiration only |
| LibreHardwareMonitor | MPL-2.0 | C#, the most complete sensor source | In a separate helper process, unmodified |

Removed from winstats: the automatic GitHub update check, remote machines and LAN sharing (they
opened network ports), and the default ping to 1.1.1.1. Because of the privacy requirement, the app
opens no network connections at all now.

## 2. Verification

| Requirement | Result | How |
|---|---|---|
| Starts without problems | ✅ | Log, screenshots, real use |
| Real CPU and RAM values | ✅ | `--dump` output |
| Consistent with Task Manager | ✅ CPU 1.7 points apart, RAM 0.0 points (20 s average) | `scripts\compare-taskmgr.ps1`, against Windows' counters at the same time |
| Network rate follows real traffic | ✅ | Comparison test (5.2 / 5.2 KB/s) and the live strip |
| Widgets can be dragged into order | ✅ | Reordered by the user in real use (4 reorders in the log) |
| Settings survive a restart | ✅ | Same order and settings after restarting |
| Recovers when Explorer restarts | ✅ | explorer.exe killed and started; the strip reattached within 1 s |
| No fake values for unsupported sensors | ✅ | Intel GPU temperature and CPU temperature without PawnIO show "not available"; driver-only zeros are filtered out |
| Taskbar normal after uninstalling | ✅ | `--install --quiet` / `--uninstall --quiet` cycle; files, shortcut and registry entry removed |
| Idle CPU < 1% | ✅ **0.05% of the machine** (0.81% of one core) over 304 s | Sampling with `Get-Process` |
| Memory | ✅ 16.1 MB private on average (20.3 MB peak), working set about 42 MB | Same measurement |
| Sensor helper | ✅ PawnIO 2.2 + administrator: 147 readings, 36 temperatures, CPU 65 °C | Log |

Automated tests: 22 unit tests (config, locales, units, notification rules, taskbar fitting,
sensor protocol, DirectWrite text, JSON output) and `compare-taskmgr.ps1`.

## 3. Done

- **Taskbar:** a strip embedded in the taskbar. When Windows refuses that, an automatic floating
  mode takes over and embedding is retried every minute. The Windows 11 task list is watched through
  UI Automation so the strip never covers it. Recovers by itself when Explorer restarts.
- **Modules:** CPU (core bars, P/E split, clocks, Task-Manager-compatible load, processes), GPU
  (load, VRAM, driver, several adapters, temperature/fan when reported), RAM (usage, commit,
  breakdown, SMBIOS modules, processes), Disk (read/write, physical disks, SSD/HDD/NVMe, temperature,
  drive usage, I/O processes), Network (download/upload, adapters, Wi-Fi, IP/MAC, totals, adapter
  selection), Battery (hidden automatically on desktops), Sensors (temperature widget and panel).
- **Design:** sharp text through DirectWrite (Segoe UI Variable, tabular figures), antialiased area
  charts, a Stats-style mirrored network chart, a settings window in the style of Windows 11
  Settings, system/dark/light themes, an accent palette, high-DPI awareness (Per-Monitor V2).
- **Settings:** modules and order, colours, theme, text and chart sizes, interval (0.5/1/2/5 s),
  position, placement mode, start with Windows, threshold notifications, sensors, privacy, about and
  licences.
- **Performance:** measuring on its own thread; details collected only while a panel is open; bounded
  history (360 samples); sleep/resume handled; event-driven taskbar watching.
- **Product:** per-user install and uninstall (no administrator rights), crash log and Windows'
  automatic restart, atomic config saving, build script, licence notices.

## 4. Missing and known limitations

- **Not tested on Windows 10.** The Windows 10 code path (narrowing the task list) comes from winstats
  and is kept, but no Windows 10 machine was available.
- **Several monitors:** the strip is shown on the main taskbar only. The panels and the settings
  window follow the DPI of their monitor; secondary taskbars have no strip.
- **Vertical taskbar** (the layout from winstats) was not tested: Windows 11 does not officially
  support one.
- **Other languages:** only English and Turkish are complete. In the 27 other languages from
  winstats, new strings appear in English.
- **Notifications** appear on Windows Stats' own card, not in the Windows notification centre.
- The **sensor helper** uses about 80–100 MB of extra memory while on (.NET + LibreHardwareMonitor),
  so it is off by default. Administrator mode asks for UAC at every start (starting it without a
  prompt through a scheduled task was left out, as that is a system-wide change).
- On this mini PC **no fan speed** could be read: the hardware exposes no readable Super I/O chip,
  so the value stays "not available".
- **Unsigned executable:** antivirus products such as Kaspersky may flag new, unsigned builds by
  heuristics (the build folder needed an exclusion during development). A code-signing certificate is
  recommended for distribution.
- During development the Insider build's taskbar temporarily refused child windows. The automatic
  floating mode covered that, and after a restart embedding worked again.

## 5. Privacy and security summary

No telemetry, no network ports, no internet required, no administrator rights requested (UAC only
when the user turns it on for the sensor helper), no drivers installed (the user installed PawnIO
themselves), only `HKCU` keys and user folders are used.

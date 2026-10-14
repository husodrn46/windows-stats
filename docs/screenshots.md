# Real application screenshots

Capture on a Windows 10/11 machine. Linux cross-compilation cannot provide genuine taskbar or hardware screenshots. No screenshot placeholders are presented as application output.

Build the current source using the README instructions. For each capture use a new, absolute scratch directory under `.devtmp`; set `WINDOWS_STATS_HOME` to that exact path and `WINDOWS_STATS_DEBUG_DUMP` to an existing output directory. Never point either variable at the normal `%APPDATA%\WindowsStats` directory. Start only this test build; never close the user's installed copy.

`WINDOWS_STATS_DEBUG_POPUP=cpu|gpu|mem|disk|net|battery|sensors` opens the corresponding visible widget after the fourth collector sample when both scratch-home and dump variables are present. Enable the desired widget in the scratch settings first. Wait for the detail data to arrive before choosing a dump. Dumps overwrite on redraw:

- `strip.bmp`: application strip only.
- `popup-<kind>.bmp`: application's own popup canvas only.
- `settings-<n>.bmp`: settings page, selected with `WINDOWS_STATS_SETTINGS_PAGE=<0..6>` and `--settings`.

Use `popup_opacity = "solid"` in the scratch configuration for a self-contained panel image. Do not enable elevated sensors or install drivers for a capture without the user's permission. Unsupported sensors must remain unavailable; do not invent readings.

Example in a dedicated PowerShell session (run from the repository root):

```powershell
$env:WINDOWS_STATS_HOME = Join-Path $PWD '.devtmp\capture-cpu-home'
$env:WINDOWS_STATS_DEBUG_DUMP = Join-Path $PWD '.devtmp\capture-cpu-dump'
New-Item -ItemType Directory -Force $env:WINDOWS_STATS_HOME, $env:WINDOWS_STATS_DEBUG_DUMP | Out-Null
$env:WINDOWS_STATS_DEBUG_POPUP = 'cpu'
Start-Process .\target\release\WindowsStats.exe
# Wait for readings and inspect the BMP before conversion.
# Close ONLY this isolated instance with the same exact scratch home:
.\target\release\WindowsStats.exe --quit
python -I tools/bmp32topng.py .devtmp/capture-cpu-dump/popup-cpu.bmp .devtmp/popup-cpu.png
```

Review even canvas-only dumps: panels can contain process names, network IP/MAC addresses, Wi-Fi names, hostnames and hardware identifiers. Do not publish those details without review. Prefer disabling relevant optional rows in the scratch configuration; otherwise redact the image and label it as redacted.

After review, copy PNGs to `docs/images/`: `strip.png`, `popup-cpu.png`, `popup-mem.png`, `popup-net.png`, `popup-disk.png`, `popup-sensors.png`, `settings-0.png` through `settings-6.png`. Add the actual image links to both READMEs only after these files exist.

The social preview should be a 1280×640 PNG made from these approved captures and the title “Windows Stats”. With Pillow available (`python -m pip install Pillow` in your development Python environment), run:

```text
python tools/make-social-preview.py docs/images .devtmp/social-preview.png
```

The composer requires real `strip.png`, `popup-cpu.png`, `popup-net.png` and `settings-0.png` files; it stops if any are missing. Review the result and upload it manually using the repository's **Settings → Social preview**. Do not use an invented interface or synthetic hardware data.

# Bluetooth accessory batteries (first version)

The Battery panel now lists connected Bluetooth accessories for which Windows exposes a
valid battery percentage. Settings → Widgets → Accessory Batteries lets you pin up to two
accessories alongside the existing system-battery readout. Pins survive disconnect/reconnect;
remove an offline pin from that settings section to free a slot. No accessories are pinned
by default. On a desktop PC without a system battery, the Battery widget is offered once an
accessory is pinned, and the first pin adds it to the strip. Until then nothing is drawn for
accessories: connected but unpinned devices are listed only in Settings > Widgets.

## Limits and privacy

- Bluetooth only: no USB receiver, HID++ or other vendor protocol in this version.
- Cached Windows properties, not direct battery measurements. Device/driver support varies.
  Missing, invalid, unavailable or disconnected readings are omitted, never shown as 0%.
  A real reported 0% is valid. Charging state is not inferred from battery percentage.
- A dedicated worker checks every 30 seconds; connection changes can take a poll to appear.
  There is at most one request in flight. OS calls cannot block the metric collector or UI;
  readings expire after 90 seconds without a result. Suspend clears readings and rejects
  any outstanding pre-suspend result.
- Device names are shown locally, and stable container IDs (or node IDs when no container
  is available) are stored only for chosen pins in local config. No network, telemetry,
  new MCP output, drivers, administrator prompt, device writes or radio discovery.
- Multiple service nodes in one container are collapsed. If the OS supplies conflicting
  values, the lowest reported value is retained; it is not an averaged estimate.

## Implementation

Independent Rust implementation; no KrisGFX GPL source was copied. Uses cfgmgr32 read-only
Configuration Manager APIs and present Bluetooth enumerators (`BTHENUM`, `BTHLE`,
`BTHLEDEVICE`), validates devnode status and property type/length, and bounds allocations.

The battery property `{104EA319-6EE2-4701-BD47-8DDBF425BBE5}, 2` is an observed Windows
Bluetooth property, not a public Microsoft stability guarantee. It is **not** the documented
Shell `System.Devices.BatteryLife` key (which has a different GUID). Unsupported Windows
builds/drivers simply return no readings. Real device acceptance is necessary.

References:
- [Microsoft: enumerate installed devices](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/enumerating-installed-devices)
- [Microsoft: CM_Get_DevNode_Status](https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_get_devnode_status)
- [Microsoft: CM_Get_DevNode_PropertyW](https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_get_devnode_propertyw)
- [Microsoft: Bluetooth battery profiles](https://learn.microsoft.com/en-us/windows-hardware/design/accessory-guidelines/bluetooth-accessory-guidelines-battery-profiles-services)

## Acceptance checklist

Automated portable tests cover valid 0/100, invalid/unknown levels, duplicate nodes,
disconnection/reconnection identity, expiry and in-flight suspend invalidation. Config/widget
unit tests cover old config migration, persistence, two-pin limit, repeated toggle and
Unicode labels. A cross-compile or test-executable build does not execute Windows tests.

Before release, use a separate `WINDOWS_STATS_HOME` on Windows 10/11 and verify:
1. Desktop with no system battery: no Battery widget until a pin; Settings > Widgets lists the
   accessories (empty state readable); the first pin adds the widget, removing the last pin leaves it empty.
2. Laptop battery alone, with one accessory, then two pins; existing laptop display intact.
3. Compare at least one classic and one LE device against Windows Bluetooth settings.
4. A third device is still listed but cannot displace either pin; offline pins can be removed.
5. Disconnect/reconnect, Bluetooth off/on, sleep/wake; no stale percentage restored on wake.
6. Empty/many-device lists (four entries per page), long names, EN/TR, light/dark, DPI and vertical taskbar.
7. CPU/memory overhead with no Bluetooth and with devices; repeated Settings changes; Explorer
   restart; no battery device or OS API delay hangs the strip.

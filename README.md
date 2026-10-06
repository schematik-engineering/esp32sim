# C3 full-BLE oracle builds

The exact images we run in esp32sim `--ble full` (PR #195, head `1a427d57`), for the like-for-like check proposed in mikroverk/esp32sim#189. Each directory has the four flash images with `SHA256SUMS`, and the emulator's `--ble-observe` output (`emulator-ble-observe.txt`, 10 modeled seconds) and console (`emulator-console.txt`). AdvA in the emulator output is the simulator's default address; a real module advertises its own.

## advertiser/ (the CI fixture in #195)

Same bytes as `web/wasm/fw/public/c3-ble-*.bin` on the PR branch. Built reproducibly from `examples/c3-ble-advertiser` with ESP-IDF v5.5.5 and GCC esp-14.2.0_20260121; recipe and licence notice in the PR (`examples/c3-ble-advertiser/README.md`, `NOTICE.txt` here). NimBLE, legacy ADV_SCAN_IND, name `esp32sim`, 16-bit service `180f`, flags `0x06`, programmed interval 100 ms.

Emulator: 95 events / 285 PDUs in 10 s, channel order 37→38→39 each event, event spacing 100.0–110.0 ms (median 105.6 ms: 100 ms plus the 0–10 ms advDelay), no scan-response data configured.

```sh
esptool.py --chip esp32c3 write_flash 0x0 bootloader.bin 0x8000 partitions.bin 0x10000 app.bin
```

## server/ (unchanged Arduino BLE Server example)

`libraries/BLE/examples/Server/Server.ino` from arduino-esp32 **3.3.11** (ESP-IDF **5.5.5**), unmodified, FQBN `esp32:esp32:esp32c3` with default menu options (4 MB flash, default partition scheme, CDC on boot disabled), built with arduino-cli 1.5.1. Legacy ADV_IND with 128-bit service `4fafc201-1fb5-459e-8fcc-c5c9c331914b`, flags `0x06`, connection-interval range `6:18`; scan response carries the name `BLE Server Example`; readable characteristic `beb5483e-36e1-4688-b7f5-ea07361b26a8` = `Hello World says Neil`.

Emulator: 149 events / 447 ADV_IND in 10 s, channel order 37→38→39, event spacing 60.0–70.0 ms (median 65.6 ms: 60 ms plus advDelay). With `--ble full` alone there's no scanner, so the scan response appears only as `[ble-config]` (configured data); SCAN_REQ/SCAN_RSP and connections are milestone B.

```sh
esptool.py --chip esp32c3 write_flash 0x0 bootloader.bin 0x8000 partitions.bin 0xe000 boot_app0.bin 0x10000 app.bin
```

## Emulator commands

```sh
esp32sim-c3 --rom esp32c3_rev3_rom.elf --boot rom --flash-mb 4 --no-dump \
  --bootloader bootloader.bin --ptable partitions.bin --app app.bin \
  --ble full --ble-observe --max-seconds 10
```

`[ble-air]` lines: `hus` = modeled time in half-microseconds, channel, PDU type, AdvA, decoded AD fields and raw PDU bytes.

## Our own hardware note

On a C3 rev v0.3 (QFN32) the server/ images boot with the same console lines, advertise the same UUID, return the same scan-response name to a macOS central, and serve the same characteristic value, including after a real supervision timeout and reconnect. macOS doesn't expose AdvA, channel or per-event timing, so interval and channel order still need a proper scanner/sniffer, which is what your comparison adds.

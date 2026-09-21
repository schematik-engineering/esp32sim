# Arduino Wi-Fi compatibility fixture

The unchanged sketch scans for `esp32sim`, connects using the synthetic password
`esp32sim-pass`, and prints its DHCP address. It contains no simulator hooks.
Compile with the selected hardware board's normal Arduino configuration. The Schematik
integration builder uses its canonical YAML board settings and captures every compiler
flash file and offset:

```
node scripts/esp32sim/build-firmware.mjs /tmp/esp32sim-wifi --wifi esp32c3 esp32c6
```

The resulting per-chip `artifact.json` files contain `artifacts` entries with base64
`data`, `filename`, and byte `offset`, plus `flashBytes` and `psramBytes`. They sit beside
`.pio/build/serial/firmware.elf`. Run from this emulator repository:

```
cargo build --release -p esp32sim
node tools/wifi-test.mjs /tmp/esp32sim-wifi /path/to/roms
cargo build --release --target wasm32-unknown-unknown -p esp32sim-wasm
node tools/wifi-wasm-test.mjs /tmp/esp32sim-wifi /path/to/roms
```

Both runners exit nonzero unless scan finds the AP and firmware reports DHCP address
10.0.2.15. The native runner writes serial and PC/MMIO diagnostics under each firmware
project's `wifi-run` directory. `ESP32SIM_WIFI_SECONDS=2` shortens native diagnostic runs;
the default acceptance window is 30 emulated seconds. The WASM runner configures the
virtual AP through `esp32sim_wifi` and never patches symbols or firmware.

## Initial evidence

Using Arduino 3.3.9 on C3 and 3.3.8 on C6, both reach `WIFI:BOOT` but stall inside
`WiFi.mode(WIFI_STA)`, before `WIFI:SCAN_START`.

- C3 originally spends 99.5% of 30 seconds in `ram_pkdet_vol_start`, polling
  0x6000e050 bits26:24 for7. The firmware writes the same analog I2C register layout
  already implemented for S3. Sharing that model and supporting its second host moves
  execution beyond this loop.
- C3 then polls 0x60006174 bit16 in `ram_iq_est_enable`. It starts IQ estimation via
  0x60006144 bits0/1 and later consumes correlation registers0x148..0x154.
- C6 emits three `pll_cal exceeds 2ms` errors and polls0x600a0418 bit22 in
  `txdc_cal_new`. Firmware clears and sets startbit0, then reads comparatorbit29 and
  other result bits. This differs from S3/C3's RF map.

These are radio calibration prerequisites, not evidence of scan, authentication,
association, or DHCP support. The shared I2C model retains S3's deterministic ideal-radio
status approximation; it does not simulate analog calibration timing or physical RF.
The installed Espressif SoC headers document analog-master locations and command fields
but omit these RF result semantics. Further work needs frame/DMA and interrupt models
in addition to calibration completion.

## Register-driven calibration progress

A deterministic ideal-radio model now advances both specimens through calibration into
MAC `hal_init`, without firmware stubs. Each operation clears its previous completion
on a new register start edge and finishes after80 APB ticks. Zero correlation/comparator
results match the S3 model's ideal radio approximation. This models the driver's digital
handshake; physical RF quality and calibration timing are outside this approximation.

| Chip | Operation | Start register/bits | Completion register/bits |
| --- | --- | --- | --- |
| C3 | IQ estimate | 0x60006144 bits0/1 | 0x60006174 bit16 |
| C6 | TX DC | 0x600a0418 bit0 | 0x600a0418 bit22 |
| C6 | Power detection | 0x600a0810 bit0 | 0x600a0814 bits16:14=7 |
| C6 | Channel frequency | 0x600a00c0 bit14 | 0x600a00cc bit8 |
| C6 | IQ estimate | 0x600a0474 bits0/1 | 0x600a04a0 bit16 |

Native two-second runs now stop at the MAC initialization handshake: C3 polls
0x60033d14 bit0; C6 polls0x600a4ddc bit0. C6 still prints three early PLL calibration
timeouts. Neither fixture yet reaches scan, association, or DHCP.

## C3 station support

The unchanged C3 Arduino3.3.9 specimen now prints `WIFI:AP_FOUND` and
`WIFI:IP:10.0.2.15` in both native and real WASM runs. It uses the shared virtual AP,
real MAC TX/RX descriptors, interrupt events, WPA2 frames, AES GDMA key unwrap and
DHCP packets. The C3 GDMA register mapping follows the official SDK's channel offsets
and combined RX/TX interrupt words. A FIPS AES-128 block test verifies DMA ciphertext,
descriptor ownership and interrupt acknowledgement through those physical offsets.

```
ESP32SIM_WIFI_SECONDS=8 node tools/wifi-test.mjs /tmp/esp32sim-wifi /path/to/roms esp32c3
node tools/wifi-wasm-test.mjs /tmp/esp32sim-wifi /path/to/roms esp32c3
```

C6 still needs its distinct MAC layout. C3 external networking protocols, continuous
throughput, Wi-Fi reconnect behavior and hardware-specific RF modes remain unverified.

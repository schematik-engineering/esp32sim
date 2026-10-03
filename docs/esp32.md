# Classic ESP32

The `esp32` crate models the dual-core LX6 ESP32-D0WD ECO3. It boots the real
rev-3 mask ROM through the second-stage bootloader into an unchanged application.
The shared Xtensa core enables DFP context registers only for this target.
Double-precision arithmetic and PSRAM are not modeled.

```sh
cargo +1.99.0 build --release -p esp32sim
./target/release/esp32sim --chip esp32 --board esp32dev --boot rom \
  --rom web/wasm/fw/esp32_rev300_rom.elf \
  --bootloader web/wasm/fw/public/classic-hello-bootloader.bin \
  --ptable web/wasm/fw/public/classic-hello-ptable.bin \
  --app web/wasm/fw/public/classic-hello_world.bin --max-seconds 3 --no-dump
```

`hello_world_classic` compares the ROM banner, bootloader and application output
and instruction count against committed goldens. The ROM comes from Espressif's
ROM ELF package; set `ESP32SIM_ROM_DIR` to its absolute directory for tests.
The three firmware binaries were built from the existing CC0 hello_world source
with ESP-IDF 5.5.4, the pioarduino 55.03.38-1 platform and the esp32dev board.

The core includes DRAM, IRAM, RTC RAM, flash MMUs, DPORT interrupt routing and
secondary-core control, eFuse, SPI flash, three UARTs, GPIO/IO_MUX, RTC control,
and timer groups including LACT. `none`, `bare` and `esp32dev` are bare modules.
S3 board names are rejected. WASM accepts the `esp32` chip identifier.

Timer-group T0/T1 reuse the shared 54-bit counter model; the classic hardware
has 64-bit counters. LACT sleep-time RTC stepping and per-core watchdog resets
are not modeled. Peripheral extensions are separate changes in the stack.

# EX220: classic ESP32 application host devices

Base `4d02081aae1b99b17e0dc9792a6bbe65782ff7ad`, local product integration branch `classic-app`. No push. This extends EX199's classic register models with the host contracts implemented by EX210, EX213, EX214 and EX216. The existing S3/C3/C6 code and preserved EX199 adapters were inspected. No execution-speed or hardware-timing claim is made.

Classic now delivers APB/AHB UART writes and pin-routed RX bytes to BoardModel, including its 20-bit baud divider and APB/reference clocks. It implements bounded Ethernet relay queues, preserves relay mode across reset, bypasses the internal network in relay mode, and exposes raw ADC counts and completed-conversion observations. ADC streams reuse AnalogInputs::convert. GPIO callbacks carry instruction timestamps; board input edges and released inputs reach the chip, and external levels survive reboot. I2C devices retain their physical SDA/SCL route; SPI receives physical signal masks and only low driven chip selects. GPIO snapshots expose classic IO_MUX pulls. Existing LEDC and RMT models remain in use.

I2S adds a classic register-layout adapter around shared PCM packing, sample clocks and source queues. Standard master RX on two ports writes native owned descriptors, raises EOF/error interrupts, observes DPORT gates, and retains host sources across reset. It does not implement classic PDM, APLL, I2S TX, arbitrary RX EOF thresholds independent of descriptor size, or ADC continuous/DMA. Camera is outside esp32dev scope. These limits prevent a claim of full device parity.

## Reproduction and gates

Use Rust 1.99.0 per command; do not change the default toolchain. ROM_DIR is the absolute directory containing the licensed Espressif ROM ELFs.

```sh
cargo +1.99.0 test -p esp32 --test host_devices
cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
cargo +1.99.0 clippy --release --target wasm32-unknown-unknown -p esp32sim-wasm --features jit-tests -- -D warnings
ESP32SIM_ROM_DIR="$ROM_DIR" cargo +1.99.0 test --release --workspace -- --include-ignored --skip external_
RUSTUP_TOOLCHAIN=1.99.0 tools/wasm-build.sh
ESP32SIM_ROM_DIR="$ROM_DIR" RUSTUP_TOOLCHAIN=1.99.0 node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki c6-contiki-net c6-rpl-net panel
node tools/check-evidence-privacy.mjs
```

Six new focused tests cover instruction timestamps, same-cycle GPIO feedback, release/pulls/reboot, UART APB/AHB TX and pin RX, 9600/115200 baud, relay bounds/drain/reset, raw ADC observations, I2S descriptor bytes/ownership/EOF, and SPI physical routes/CS polarity. Existing classic LEDC and RMT tests remain. Final native and WASM Clippy pass with warnings denied. The release workspace suite passes 673 tests, including ignored tests and excluding external_* tests. WASM build and the eight requested Node scenarios pass. Privacy check passes; artifact/log hashes are in receipt.json.

## Product acceptance and negative results

The separate private add-on was built against this implementation. Its native suite passes 277 tests and the current app contract passes 9/9 with firmware output enabled. Fresh Arduino 3.3.8 firmware on esp32dev passes serial echo, GPIO/ADC/LEDC, BME280/BMP280/BH1750 I2C routes, ST7789 SPI output, RMT strips, and stereo I2S PCM through a disposable copy of the app adapter. UART GPS firmware reaches READY but still times out waiting for a fix after one correction. Wi-Fi scans the AP, emits Ethernet and accepts host RX, but the full relay-server DHCP check fails session validation after its setup retry. Neither is reported as a full pass.

The copied app needs one additional chip-map entry for esp32; the original app is read-only. Private firmware and app source are not included in this emulator evidence. The local product report and reproduction files remain under /tmp. This is representative acceptance, not the full S3/C3/C6 fixture matrix.

Retained failures: initial compile used root paths for BoardEdge/SpiPins and an unavailable ApConfig default; corrected imports/parser. Initial UART TX route incorrectly required a hardware output-enable signal, then 9600 baud exposed the shared 12-bit divider mismatch. A classic 20-bit divider was added with a regression check, but GPS firmware still fails. Initial Node WASM checks omitted ROM_DIR; the single retry supplies it. The SPI fixture initially compiled an alternate driver's source without that dependency; restricting it to the selected Adafruit source fixed the build. LED assertions initially expected RGB arrays instead of the adapter's packed integers; corrected to exact packed red/green/blue. Relay setup first lacked the copied server's dependency link, then rejected the fixture session ID; no further retry.

## Retention and privacy

receipt.json retains command outputs' hashes, test counts and the emulator WASM hash. Logs stay outside Git. Only task-relevant aggregate results are committed. No process inventories, personal paths, host identifiers or private firmware captures are retained here. No speed conclusion is inferred from test durations.

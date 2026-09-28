# Running the emulator in the browser (WebAssembly)

The whole emulator — both Xtensa cores, the SoC, the boards, the virtual WiFi and subnet —
compiles to a single WebAssembly module and runs inside the page, in a Web Worker. Nothing is
uploaded anywhere: firmware is read from the visitor's disk (or fetched from files you host next
to the page) and executed in the tab.

## Build and try it

```sh
tools/wasm-build.sh                      # -> web/wasm/esp32sim.wasm (needs the wasm32-unknown-unknown target)
tools/fetch-demo-assets.sh               # mask ROM ELFs, xterm.js, the Linux image (--no-linux skips its 16 MB)
python3 -m http.server -d web 8790       # any static server; file:// will not do (workers, fetch)
open 'http://127.0.0.1:8790/run.html?wasm&fw=hello'
```

Every demo in `web/wasm/fw/demos.json` boots from the mask ROM, which is not committed: without the
fetch the page stops at `esp32s3_rev0_rom.elf: 404`. xterm.js is only for the Terminal tab.

`?wasm` switches `web/run.html` from its WebSocket transport to the worker; the page gains a
firmware panel: board, flash/PSRAM size, an optional WiFi spec, function stubs, and file inputs
for the mask-ROM ELF, `bootloader.bin`, `partition-table.bin`, the app image, its ELF (symbols
— needed for stubs) and a script. **Boot** starts it; the rest of the page — console tabs,
display, touch, buttons, knob, audio, camera — is the same UI the native emulator serves.

For your own demos, `?wasm&fw=<name>` loads `web/wasm/fw/<name>.json` and boots it without
clicking (format in `web/wasm/fw/README.md`). Everything in that directory except the manifests
and `public/` (our own demo firmware) is git-ignored: the mask ROM is Espressif's and other firmware
is whoever built it; host them only where you may.

## On GitHub Pages

`.github/workflows/pages.yml` builds the module on every push to `main`, runs
`tools/fetch-demo-assets.sh` (the mask-ROM ELFs from the Apache-2.0 `espressif/esp-rom-elfs` release,
xterm.js, the Linux image), and publishes `web/` — so the page at
**https://joakimeriksson.github.io/esp32sim/** is the emulator, with the demos in
`web/wasm/fw/demos.json` — hello_world, the Touch-LCD-4B energy panel with its SID player, the Atech
Pocket Synth, and the C3 and C6 demos — one click away and the file inputs for anyone's own firmware. It
also runs `tools/fetch-pocket-tank.sh` for the **pocket-tank** demo ([mediacutlet/pocket-tank](https://github.com/mediacutlet/pocket-tank),
MIT): it checks the committed bootloader, partition table and app in `web/wasm/fw/public/` (the bytes of
the project's browser installer, whose host answers GitHub's runners with something else) and fetches the
7.56 MB model from the repository, all pinned by SHA-256. The workflow also fetches the **Linux-on-esp32-S3** flash image
(GPL-3.0, [svermigo/Linux-on-esp32-S3](https://github.com/svermigo/Linux-on-esp32-S3), release 0.7,
pinned by commit and SHA-256 in `pages.yml`) so the `linux` demos boot it — `linux-term` opens on the
xterm.js terminal tab (manifest `terminal: true`), where `vi`, `top` and colours render properly; the image is never
committed here — the source for everything in it is that repository. On a `github.io` host the page starts in wasm mode without
`?wasm`. Firmware committed under `web/wasm/fw/public/` is ours, except pocket-tank's three MIT parts next to its license; the panel is a
separate build with placeholder `secrets.h` values (checked with `strings` against the real ones).

**Demo data without a rebuild.** The panel firmware has a `demo` data partition (0x610000,
64 KB); when it holds a JSON document the firmware renders that — prices for today and
tomorrow, hourly kWh, tile states, header power, a fixed clock — and never starts WiFi. The
manifest writes `public/energydata.json` there (`flash_at`), so changing the demo is editing a
JSON file; real boards have the partition erased and behave as before. Natively:
`--flash-at 0x610000=web/wasm/fw/public/energydata.json`.

## What it is

`tools/wasm-test.mjs` runs the built module under Node through the same manifests the page
uses and fails on a panic or a missing console line; CI runs it after the goldens.

Every chip is in the one module: `esp32sim_new` takes a board name, and `esp32c3` or `esp32c6`
builds the RISC-V machine instead of the Xtensa one. The C3 has no `WebServer` of its own — it is
console-only — so the wasm layer turns its console into the same `{"t":"serial"}` messages the
S3 sends, and `esp32sim_cpu_hz` tells the worker which clock to pace against (240 MHz vs 160).

```
web/run.html     the UI, unchanged; `link` is either a WebSocket or the worker
web/emu.js       page side: firmware panel, manifest loading, window.EmuLink
web/wasm/worker.js   owns the wasm instance, paces it to wall time, relays the UI protocol
wasm/            the crate: a C ABI over esp32s3::Machine (esp32sim_new / load / wifi / stub /
                 boot / run / out_* / in_*); no bindgen, no dependencies
```

Inside the module the machine talks to the page through the same `WebServer` the native build
uses, in **queue mode**: every `send_text`/`send_binary` lands in an outbox the worker drains
after each run slice (`docs/web-ui.md` is the protocol on both sides). The worker keeps the
machine at wall time: it computes the cycle count the clock has earned, runs in ≤2 M-cycle
slices, yields every 25 ms so frames and audio flow, and resynchronises instead of bursting if
the tab falls half a second behind. `Date.now()` is passed in for the emulated SNTP server.

## What works, measured (M-series Mac, Chrome)

| firmware | in the tab | notes |
| --- | --- | --- |
| IDF hello_world | real time | ROM → bootloader → app, `esp_restart` reboots through the ROM |
| **Linux 6.11 on the ESP32-S3** (svermigo/Linux-on-esp32-S3) | real time, ~4× under Node | the esp-hosted network adapter on core 0, Linux XIP from flash on core 1, cramfs root, jffs2 overlays; login and a shell typed on the page console (UART0). Needs the `wifi` spec (PHY calibration) and a stub on `nimble_port_init` — no Bluetooth baseband is modelled, so BLE provisioning is skipped |
| Waveshare Touch-LCD-4B energy panel + SID player | **real time**, ~62 Minsn/s | LVGL at 60 fps, touch, the tune plays through WebAudio |
| Atech 14-port synth | real time | ST7735 and WS2812 decoded, buttons/knob, scripted scenario |
| ESP32-C3 hello_world | real time | the other chip: one RV32IMC core, console only — pick board `esp32c3` |
| ESP32-C6 hello_world | real time | the newest chip: one RV32IMAC core, console only — pick board `esp32c6` |
| ESP32-C6 802.15.4 energy scanner | real time | the Waveshare ESP32-C6-LCD-1.47: LVGL spectrum on the ST7789 over SPI2+GDMA, WS2812, energy detect from the MAC model's moving 2.4 GHz picture; BOOT on the page — board `waveshare-c6-lcd147` |
| ESP32-C6 Contiki-NG | real time | Contiki-NG as an unmodified IDF app: its own scheduler and 802.15.4 stack over the emulated MAC |
| …two of them on one medium | real time | a manifest with a `nodes` array boots a network through `esp32sim_net_*` instead of `esp32sim_new`: several motes, one medium, no simulator behind them |
| …an RPL/IPv6 network | real time | rpl-udp server (the DAG root) and client: the client joins, then UDP request and reply every 10 s over 6LoWPAN, RPL Lite, CSMA and hardware ACKs |

The browser's Xtensa block scheduler can compile hot integer/branch/memory blocks into
additional WASM modules. After 32 executions, an eligible block is installed in the emulator's
exported function table. Subsequent calls stay inside WASM; JavaScript compiles and retires
modules but does not dispatch each block. Calls have generated WASM paths; window overflow
and illegal calls retain the existing exception handling. A supported prefix ending in a
return can also be compiled: the return uses the existing interpreter helper, preserving
its window and exception semantics. Other unsupported operations keep their block interpreted.
Unaligned, unmapped, read-only and peripheral accesses use the ordinary bus helper.

Compiled WASM blocks can survive decoded-arena turnover. Reuse requires matching every decoded
instruction, the block length and the fast-memory contract. At each arena flush, retention is
limited to the two most recent decoder generations, 16,384 blocks and 64 MiB of emitted WASM
per core. New code can grow beyond those retention limits between flushes; engine-generated
machine code and other browser allocations are additional. The host reports peak live emitted
bytes and module counts to make this tradeoff measurable.

Whole-block calls use a separate path when entry checks prove there can be no register-window
collision and no active loop end within the block. That path omits per-instruction entry,
budget, overflow and loop-end tests. Generated code loads only the block's operand registers
and computes register-window collision state once per entry. Budget cuts, resumptions and
states that fail either guard keep their checked path.

Once a block is hot, the emitter also tries to form a *region*: the block plus the blocks
reachable from it over statically known edges (fallthrough, conditional-branch target, `J`,
and the backedge of a hardware loop set up inside the region), compiled as one function.
Formation starts with at most 8 chunks, 64 instructions and 4 code pages; splitting at
hardware-loop ends can add chunk boundaries without adding instructions. Guest registers stay in WASM locals
across the internal edges; continuing inside the region checks the remaining instruction
allowance and whether a helper or code-page store requires an exit. Anything that
could make a block boundary observable leaves the region instead: interpreter helpers and
stores into one of the region's own code pages make the next chunk head exit, calls, returns
and computed jumps end a chunk, and entry checks cover page versions, probe boundaries, an active hardware loop,
and window and coprocessor state. A region's continuation
exits carry the next PC and are never mid-block cuts, so budget cuts inside a chunk still go
through the chunk's own block module. A dispatch at any chunk head of a live region enters the
region at that chunk, and a head already inside a live region does not get a region of its
own. `ENTRY` may head a region; the window proof is redone after the rotation. The profile
build (`jit-profile`) reports regions formed, entries, generated-entry rejects and instructions retired
per core.

The PIE (coprocessor 3) instructions the TinyDraw tile kernels use are emitted on WASM SIMD:
aligned 128-bit load and store with post-increment, lane compares, the bitwise q-register
operations, 32-bit lane insert and zeroing. So is the dot product of on-device inference
(pocket-tank's 4-bit matmul): signed 8- and 16-bit multiply-accumulate into ACCX, with and
without its load, the ACCX reset, and the RUR of ACCX_0/ACCX_1 that follows each dot product.
The products are widening vector multiplies with 64-bit lane sums, and ACCX is updated with
the interpreter's 40-bit saturation. Q registers stay in CPU memory as `v128` values; generated
functions have one `v128` and one `i64` scratch local. The CP3-disabled check is proved once per
body next to the FP one. Other PIE instructions keep the interpreter, and a block containing
any of them stays interpreted.

Compiled execution uses the same instruction-count timing as the default block interpreter.
Timer budgets, interrupts, loop ends, code-page versions and observer boundaries still bound
execution. This does **not** extend the receipt-based cycle model or establish cycle accuracy.
The earlier `esp32sim_jit_prepare/commit` experiment remains available for its synthetic test;
the page no longer uses it to run firmware.

Custom WASM hosts must provide `host_jit_compile` and `host_jit_release` from
`web/wasm/jit.mjs`, alongside `host_log`. The module exports `__indirect_function_table`.
Append `&jit=0` to the page URL, or configure `esp32sim_set_jit(emu, 0)` before
running, to compare with the interpreter;
`esp32sim_block_jit_insns(emu)` counts retired instructions through compiled blocks, including
memory helpers. `createJitHost` exposes compilation, failure, release and compile-time counters.
`ESP32SIM_NO_WASM_JIT=1 node tools/wasm-test.mjs ...` exercises the interpreter on the same build.

`tools/wasm-jit-test.sh` builds a separate test-enabled module and runs generated-code
comparisons with the interpreter under Node, including budget/resume, timers, register windows,
loops, memory faults and code invalidation. CI runs it alongside the firmware smoke tests.
It does not overwrite `web/wasm/esp32sim.wasm`.

An optional `jit-profile` feature adds statistical block profiling to the WASM build. Its
host must supply `env.host_profile_now`, a monotonic millisecond clock (for example,
`() => performance.now()`). Call `esp32sim_profile_report(emu)` to emit per-core TSV through
`host_log`: sampled PCs, compiled/interpreted status, instruction counts, elapsed samples,
unsupported operations and block operations. Blocks are sampled with probability 1/4096
using a pseudorandom sequence. PC rows describe the first sampled decoded shape; profiles
of self-modifying code may combine multiple shapes at the same PC. Counts include trap
iterations and compiled helper execution, so they are estimates rather than exact opcode
retirement counts. Elapsed samples include lookup and dispatch but exclude the outer SoC
scheduler. Clock-call overhead and quantization can dominate these short intervals; do not
convert their sum into a wall-time breakdown. Use uninstrumented runs to measure speed.
Normal builds contain neither the sampling code nor the clock import.

## Limits

- **No NAT.** The browser has no sockets. With a `wifi=` spec the firmware still associates,
  gets a DHCP lease, resolves names and syncs time against the emulated subnet, but connections
  past the gateway are refused (the `--net none` behaviour). A WebSocket relay to a small host
  helper is the planned way out (`wasm-plan.md`).
- **No file outputs**: `--wav`, `--tft-png`, register traces — the page is the output.
- **Emulator log lines** (`[emu] …`) that the native build prints to stderr do not exist here,
  except the ones the wasm glue forwards (stubs, resets, load errors) to the console tab and the
  browser console.
- **Memory**: flash + PSRAM + SRAM + ROM plus the block caches; the panel configuration takes
  ~45 MB of wasm memory. The block tables are sized smaller than natively (`block.rs`).
- Audio needs one click on **enable audio** — browsers will not start WebAudio otherwise.

## A network of motes

A manifest with a `nodes` array boots a *network* rather than a single machine: the page calls
`esp32sim_net_new` and, per node, `esp32sim_net_add` (its MAC, board, position and power-on
offset) and `esp32sim_net_load`, then `esp32sim_net_run` advances the whole network to a point in
network time while `esp32sim_net_console_take` and `esp32sim_net_stat` report what each mote did.

```json
"nodes": [ { "mac": "02:00:00:00:00:01", "start_ms": 0,    "x": 0, "y": 0 },
           { "mac": "02:00:00:00:00:02", "start_ms": 1300, "x": 2, "y": 0 } ]
```

The medium and the lock-step are in the module (`esp32c6::net`), not in the worker, for the same
reason the Cooja front end keeps them in Rust: `Machine::run_until_cycle` stops at the instruction
that starts a transmission and `SocBus::radio_receive` puts a frame on the air at its first
preamble byte, so the exactness is already there and a native test can hold it to it
(`esp32c6/tests/net.rs`). The worker only paces network time to the wall clock and relays.

A node may carry its own `files` and `symbols`: an entry of a kind the node names replaces the
shared one, so a root and a client — two images with two `bb_init` addresses — share one
bootloader and partition table and differ only in `app`.

`start_ms` is not cosmetic. Two identical images booted at the same instant are deterministic to
the cycle, so their application timers never drift apart: every broadcast is sent while the other
mote is transmitting, and nothing is ever heard. Real motes are staggered by their power-on; here
it has to be said out loud.

## Host ADC samples

`esp32sim_set_adc(emu, gpio, raw)` sets a 12-bit input code for a physical ADC GPIO and returns zero on success, one for an unsupported pin, an out-of-range code, or a null emulator. S3 accepts GPIO1 through 20, C3 GPIO0 through 5, and C6 GPIO0 through 6. Inputs persist across firmware reboot.

The firmware still selects the ADC unit/channel and starts a conversion through SENS on S3 or APB_SARADC on C3/C6. The model latches the selected input, advances a conversion deadline, and publishes the data and completion registers. Arduino's resolution mapping runs unchanged in firmware. Raw inputs represent codes after attenuation; this interface does not convert voltages or emulate calibration transfer curves. S3 output-invert bits are honored. Continuous/DMA acquisition, completion IRQ routing and analog-clock/sample-cycle accuracy are not implemented. Conversion latency currently uses fourteen SAR clocks at the programmed divider.

The real-WASM regression consumes existing hardware firmware artifacts without rewriting firmware:

```sh
cargo build --release --target wasm32-unknown-unknown -p esp32sim-wasm
node tools/adc-test.mjs /path/to/io-fixtures /path/to/roms
```

The fixture directory contains `{esp32s3,esp32c3,esp32c6}/{artifact.json,fixture.json}` from Schematik's hardware I/O fixture builder. Each run boots the ROM and compiler-provided flash files, sends the firmware's `A` command, and checks raw readings at 0, 1024, 3072 and 4095. Register definitions follow Espressif ESP-IDF 5.5's chip-specific `adc_ll.h`, `sens_reg.h`, and `apb_saradc_reg.h`.
# Project circuit configuration

`esp32sim_configure_circuit(e, data, len)` accepts eight-byte records before boot:

- `[1, pin, count_lo, count_hi, 0, 0, 0, 0]`: WS2812 strip.
- `[2, id, SDA, SCL, address, width, height, 0]`: SSD1306 display.

The call returns zero on success and one for invalid configuration or a booted machine. Limits are sixteen strips and sixteen displays. Display IDs and `(SDA,SCL,address)` tuples must be unique. The actual GPIO matrix routing selects an I²C device; sharing an address on separate wires does not merge device state. Controller resets preserve external devices. Replacing the configured board detaches its old devices.

Changed display output uses binary message kind2 with payload `[5,id,width,height,...page-packed mono bytes]`. Each frame owns its bytes. This model implements logical SSD1306 GDDRAM addressing, power, inversion, entire-display mode and start-line offset from the [Solomon Systech specification](https://www.mouser.com/datasheet/2/813/SSD1306-3401599.pdf). Physical SEG/COM mounting, scrolling, contrast and external reset wiring are not yet modeled.

### Structured Wi-Fi configuration and observed state

`esp32sim_wifi_configure(e, ssid_ptr, ssid_len, password_ptr, password_len, channel)`
accepts literal UTF-8 credentials. Commas and equals signs have no special meaning.
SSID length is 1–32 bytes, channel is 1–14, and the password is empty for an open AP,
8–63 bytes for WPA2, or 64 hexadecimal digits for a raw PSK. It returns 0 on success,
1 for an unsupported radio, and 2 for invalid configuration. Configuration logs contain
no credentials or SSID. The old `esp32sim_wifi` comma-separated interface remains
available for existing callers.

`esp32sim_wifi_state(e)` derives the badge state from the virtual AP protocol exchange:
0 disconnected, 1 authentication/key negotiation in progress, 2 associated with WPA2
keys installed or an open AP. Unsupported chips and null instances return `u32::MAX`.
Relay readiness does not change this state. DHCP/IP acquisition is separate.

S3 and C3 support these calls. The C3 Arduino fixture passes scan, WPA2 association and
DHCP with its unchanged hardware binary through the actual WASM module. C6 radio
calibration now reaches MAC initialization, but its MAC is not yet connected to an AP.

## Host microphone PCM

`esp32sim_audio_configure(e,id,kind,port,data_gpio,bclk_gpio,ws_gpio,sample_rate,channels)` attaches PCM16LE input to a physical microphone. IDs are 0–15 per emulator instance. Rates are 8,000–96,000 Hz, with one or two interleaved channels. `kind=0` feeds an ADC GPIO; its port/BCLK/WS arguments must be zero. `kind=1` feeds standard I2S RX. S3 has ports 0 and 1; C3/C6 have port 0. For I2S, `port=0xffffffff` discovers the receiver through the firmware's GPIO matrix. Each source retains its own data/BCLK/WS wiring; several microphones on different wires can coexist. Reconfiguring an ID replaces its previous input and clears its queue. A duplicate receiver/wiring assignment is rejected.

`esp32sim_audio_push(e,id,ptr,len)` accepts complete PCM16LE frames, at most 768,000 bytes per call. Each source queue holds at most two seconds of frames and drops the oldest frames on overflow. Auto-port sources keep a separate bounded queue for each possible controller, at most two on S3. Queues continue advancing in emulated time while their controller is inactive; when the whole emulator is paused, the fixed capacity still applies. Queue underrun supplies PCM zero, which maps to ADC code 2048. `esp32sim_audio_reset(e,id)` clears queued and current samples. Firmware reboot preserves external microphone configuration, queued frames and the current sample; controller registers and DMA reset normally.

These calls return 0 on success, 1 for an unsupported or unconfigured target, and 2 for malformed input or conflicting configuration. `esp32sim_audio_info(e,id,field)` reports the active receiver's register-derived sample rate, sample width, channel count and running state for fields 0–3. Those fields are zero until a wired receiver starts. Field 4 is the observed port, or `0xffffffff` when no receiver matches. ADC sources report their host rate/format. Hosts can turn changes into their existing audio-configuration UI events without guessing which I2S controller the firmware chose.

ADC PCM advances on the emulated 80 MHz APB clock, independently of how often firmware calls `analogRead`. Stereo input is averaged to mono; signed PCM is mapped to the full raw 12-bit range with nearest rounding. Normal register-driven ADC conversion and Arduino resolution conversion still execute in firmware.

I2S RX supports standard master reception with 16-, 24- or 32-bit slots/data, mono slot selection or stereo, and the modeled crystal/PLL clock dividers. Sixteen-bit host samples are expanded into the high bits of 24-/32-bit DMA words. GPIO matrix data/BCLK/WS must match a configured source. GDMA fills firmware descriptors, writes back length/ownership/EOF and raises the chip's DMA interrupt. C6's RX clock comes from PCR; C3's GPIO input-select bit differs from S3/C6. The implementation follows ESP-IDF 5.5's chip-specific `i2s_ll.h`, `i2s_reg.h`, `gpio_sig_map.h`, `gpio_reg.h`, `gdma_reg.h` and `pcr_reg.h`. PDM, external/slave clocks, multichannel TDM, inverted/bit-reordered links and analog voltage/calibration modeling are outside this PCM path. Resampling currently holds each input sample; it does not apply an anti-alias filter.

Schematik's `tests/fixtures/esp32sim/audio/check.mjs` runs the actual WASM module against compiler-produced Arduino flash artifacts. It checks the flash-manifest hashes, ADC sample levels, stereo DMA bytes, source isolation, host reset, underrun and firmware reboot. On S3 it also switches the unchanged firmware from I2S0 to I2S1 and verifies automatic routing. The fixture uses canonical board YAML through the existing hardware fixture builder; no firmware rewriting, facade imports or C++ stubs participate.

### Camera input (ESP32-S3)

`esp32sim_camera_configure(emu, ptr, 20)` attaches one OV2640 or OV5640 to the
project board. Bytes are `[PIDlo, PIDhi, id, SDA, SCL, XCLK, PCLK, VSYNC, HREF,
D0, D1, D2, D3, D4, D5, D6, D7, PWDN, RESET, fps]`. PID is `0x0026` for
OV2640 and `0x5640` for OV5640. GPIOs must be distinct and in the chip's range;
PWDN/RESET may be 255 (unconnected). The host supplies physical project wiring,
not a firmware I2C controller number. SCCB follows the GPIO matrix on either
I2C controller. Parallel data and clock wiring must match LCD_CAM's matrix
routes. C3/C6 return unsupported.

`esp32sim_camera_push(emu, id, width, height, format, ptr, length)` replaces the
latest external frame. Host formats are RGB565 little endian (0), YUYV (1),
grayscale (2), JPEG (3), and RGB888 (4). Frames are bounded to 1600×1200 and
5,760,000 bytes; raw byte counts must match geometry. JPEG SOI/SOF/EOI and encoded
dimensions are checked. Encoded JPEG is used only while the sensor is in JPEG
mode. Raw inputs are packed into the sensor's configured RGB565/YUYV/Y8 stream.
Host dimensions must match the sensor's SCCB configuration before any bytes
reach DMA. No image is generated when external input is absent.

`esp32sim_camera_info(emu, id, field)` returns sensor width (0), height (1),
DVP format (2), or configured/streaming state (3). These are wire formats, not
`esp_camera` SDK enum values. OV2640 grayscale firmware commonly receives YUYV
and extracts luma; OV5640 also has a Y8 wire mode. Sensor configuration and the
latest host image survive a chip reboot, while LCD_CAM/GDMA are reset. The real
firmware reinitializes SCCB. `esp32sim_camera_reset(emu, id)` removes host input;
an already captured firmware framebuffer can still be queued by the driver.
Configure/push/reset return 0 on success, 1 for unsupported/missing devices, and
2 for invalid arguments.

The model supports the S3 8-bit parallel interface and existing GDMA descriptor
semantics, including byte-count EOF, ownership checks, circular SRAM buffers,
and VSYNC independent of receiver start. Frame timing uses the host's 1–30 fps
setting with a 5% vertical blanking interval and a 50% active interval; individual
DVP edges, exposure, lens/ISP effects, and JPEG encoding are not simulated. The
host supplies encoded JPEG bytes. A JPEG that exceeds the firmware's allocated
buffer or does not fill its minimum DMA chunk is rejected/timed out by the real
driver; bytes are never padded to manufacture a successful capture. 16-bit DVP
and alternate HSYNC/DE capture modes do not transfer frames.

Register behavior follows Espressif's [S3 camera driver](https://github.com/espressif/esp32-camera/blob/master/target/esp32s3/ll_cam.c),
[OV2640 driver](https://github.com/espressif/esp32-camera/blob/master/sensors/ov2640.c),
and [OV5640 driver](https://github.com/espressif/esp32-camera/blob/master/sensors/ov5640.c).

### Positional servos

`esp32sim_servo_configure(emu, id, pin, min_us, max_us, min_angle, max_angle)`
attaches a calibrated physical servo to GPIO. IDs are 0–15, pulse endpoints are
ordered integers within 100–5000 microseconds, and angle endpoints are ordered
signed millidegrees within −360000–360000. Returns 0 on success, 1 for an absent
chip pin, or 2 for invalid arguments. Invalid updates preserve the previous
configuration. Reconfiguring the same pin preserves its last position.

`esp32sim_servo_info(emu, id, field)` returns active input (field 0), rounded pulse
microseconds (1), or signed angle millidegrees (2). A valid input is a nonconstant
20–400 Hz PWM waveform with a 100–5000 microsecond high pulse. Position maps the
pulse linearly between the physical calibration endpoints and clamps outside
them. A missing signal holds the previous position; this model does not simulate
mechanical speed, inertia or electrical load. Configurations and positions survive
firmware reboot; the GPIO controller independently resets. Multiple IDs can share
a GPIO while retaining independent calibration.

The source is the actual GPIO-matrix-routed PWM output: LEDC on S3/C3/C6, plus the
S3 MCPWM up-counting generators used by ESP32Servo 3.2.1. The MCPWM model covers
clock gating/reset, timer prescalers/periods, generator A/B compare/shadow updates,
continuous force, output inversion and timer/compare interrupts. Down/up-down
counting, carrier modulation, dead time, and multi-transition generator patterns
do not produce a servo measurement. This is not a motor-control plant model.

The host must supply calibration. A UI may offer a generic adjustable
1000–2000 microsecond / 0–180 degree range, but that is not a measured range for a
specific servo. Firmware library endpoints do not automatically change physical
calibration. ESP32Servo 3.2.1 resets its timer to 10 bits on initial attach, and
its S3 detach path releases bookkeeping without stopping or disconnecting MCPWM;
those unchanged-firmware behaviors remain visible in the physical output.

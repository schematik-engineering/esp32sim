# Classic ESP32 ECO3 boot spike (EX199)

## Scope and result

This spike adds the smallest classic ESP32 target that reuses the existing Xtensa core,
SoC runner, and shared peripheral models. It tests functional boot correctness, not speed
or cycle accuracy. It differs materially from [EX008](../../experiments.md#ex008) and
[EX010](../../experiments.md#ex010): the target is the dual-core Xtensa LX6 ESP32-D0WD
ECO3, the input is its real rev-3 mask ROM plus an unchanged Arduino-ESP32 3.3.8 image,
and the contract includes ROM boot, second-stage bootloader and application GPIO output.

All three bounded milestones passed at code-under-test revision `d13362f`:

- A: the rev-3 mask ROM printed its banner, selected SPI fast-flash boot, and loaded the
  second-stage bootloader from the merged image's `0x1000` segment.
- B: the bootloader loaded the application image described by the partition table at
  `0x8000`, then execution reached `esp_startup_start_app` (`0x400f0f68`), `main_task`
  (`0x400f0ef8`), and `app_main` (`0x400d4b74`).
- C: the unchanged sketch printed `CLASSIC_BOOT` and `tick 0`; the GPIO VCD recorded
  GPIO2 rising at 18,664,800,000 ps of modeled time.

The requested example banner date, `ets Jun  8 2016`, belongs to an older ROM. The
caller-supplied ECO3 `esp32_rev300_rom.elf` correctly printed `ets Jul 29 2019 12:21:46`.

## Revisions and inputs

- Base: upstream `dddb128`.
- Implementation revisions: `3b15629` (ROM boot) and `d13362f` (Arduino application).
- QEMU behavior reference: Espressif QEMU `esp-develop` at
  `febae182fb7abb8e76a738a8b6a174b61ca627643`; read only. No GPL source was copied.
- Host tools: Rust/Cargo 1.96.0, PlatformIO Core 6.1.19, Darwin arm64, macOS 26.6.2
  build 25G83.
- Firmware platform: `platform-espressif32` `55.3.38+sha.fbdfc29`, Arduino-ESP32 3.3.8.

SHA-256 inputs and artifacts:

| Item | SHA-256 |
| --- | --- |
| `$HOME/.platformio/packages/tool-esp-rom-elfs/esp32_rev300_rom.elf` | `920b70635440517866aab2230964a570d2cf2b676658d93c52fbac108c1cca31` |
| `/tmp/esp32sim-classic-pio/platformio.ini` | `940f177931f32bc257b058a53dae12fddabb70ec3a80ea4b0e70aa0b4e55aa28` |
| `/tmp/esp32sim-classic-pio/src/main.cpp` | `8ff68737b709d4b704bed48a5965a1eccd9cb1fc8626bb4d569f674ae140ff0e` |
| `bootloader.bin` | `a227e3ab93f15f1155efb3144810cc06ff7a259e9bc9b4542417afcc6c238214` |
| `partitions.bin` | `148b959cbff1c38aa8e1d5c0ba9d612c54997b945e56a63f41223eef650653a1` |
| `firmware.bin` | `cf4632577191a5292e54677bfb782ea9bb83030e8ce71536326d13b858bd2e85` |
| `firmware.elf` | `d19b529a333d47db826bdc3c25c90ecad7b3ab6e5e0fa42e3ba765d854d35917` |
| `firmware.factory.bin` | `b0361619b04087f666fae5c5b7d6a00536062da1dd969d77f5171639f7d701a9` |

The temporary PlatformIO project used this configuration:

```ini
[env:esp32dev]
platform = https://github.com/pioarduino/platform-espressif32.git#55.03.38-1
board = esp32dev
framework = arduino
monitor_speed = 115200
```

Its `src/main.cpp` was the sketch specified for the spike. Build it with:

```sh
cd /tmp/esp32sim-classic-pio
pio run
```

The platform build emitted `firmware.factory.bin`, a 4 MiB merged image containing
`bootloader.bin` at `0x1000`, `partitions.bin` at `0x8000`, the framework's boot-app
selection image at `0xe000`, and `firmware.bin` at `0x10000`.

## Reproduction and output checks

From the repository root:

```sh
cargo build --release
target/release/esp32sim --chip esp32 --boot rom \
  --rom "$HOME/.platformio/packages/tool-esp-rom-elfs/esp32_rev300_rom.elf" \
  --flash-image /tmp/esp32sim-classic-pio/.pio/build/esp32dev/firmware.factory.bin \
  --elf /tmp/esp32sim-classic-pio/.pio/build/esp32dev/firmware.elf \
  --max-insns 20000000 --no-jit --no-reboot --no-dump \
  --vcd /tmp/esp32-classic-gpio.vcd
```

The UART transcript was:

```text
ets Jul 29 2019 12:21:46

rst:0x1 (POWERON_RESET),boot:0x13 (SPI_FAST_FLASH_BOOT)
configsip: 0, SPIWP:0xee
clk_drv:0x00,q_drv:0x00,d_drv:0x00,cs0_drv:0x00,hd_drv:0x00,wp_drv:0x00
mode:DIO, clock div:2
load:0x3fff0030,len:4640
load:0x40078000,len:15660
load:0x40080400,len:3164
entry 0x4008059c
CLASSIC_BOOT
tick 0
[emu] stop: MaxInsns — core0 4545698 + core1 825493 insns in 0.1s wall = 63.6 Minsn/s; emulated 0.083s (20000000 cycles); 3767 exceptions, 238 interrupts
[vcd] wrote 447 events to /tmp/esp32-classic-gpio.vcd
```

The VCD output contained:

```text
$var wire 1 s2 gpio2 $end
#18664800000
1s2
```

The bounded run stops before the sketch's 500 ms delay expires, so one rising edge and
the first serial line are the intended C check. It does not establish later periodicity.

Final gates on the documented source tree all passed:

```sh
cargo test --workspace
cargo build --release
tools/wasm-build.sh
node tools/check-evidence-privacy.mjs
```

The workspace run covered the new classic crate and the existing S3, C3, C6, shared
peripheral, shared SoC, CLI, and WASM tests. Tests that require external firmware remain
explicitly ignored by their existing contracts.

## GPIO and interrupt-routing extension

Revision `d64ac6e` extends EX199 rather than starting another experiment. The target,
ROM, Arduino version and functional workload are unchanged. The material difference is
the correctness contract: GPIO pad configuration, matrix routing, external edges and
peripheral interrupts must now reach the running application through the classic DPORT
matrix. This remains functional evidence, not a speed or cycle-accuracy claim.

The added register tests cover GPIO output and enable aliases, matrix output selection,
output and output-enable inversion, constant and pad input selection, IO_MUX input,
pull-up and function selection, the GPIO34 input-only rule, falling and level interrupt
status/clear behavior, both DPORT CPU maps, and delivery from GPIO, UART, timer, timer
watchdog and RTC watchdog sources. They also cover the classic UART FIFO pointer status
used by Arduino's receive path.

The temporary validation sketch was built outside the repository with the same
`platformio.ini` shown above. Its source was:

```cpp
#include <Arduino.h>

volatile unsigned edges;

void IRAM_ATTR on_falling() {
  ++edges;
}

void setup() {
  Serial.begin(115200);
  pinMode(4, INPUT_PULLUP);
  attachInterrupt(4, on_falling, FALLING);
  Serial.printf("initial=%d\n", digitalRead(4));
}

void loop() {
  static unsigned sample;
  Serial.printf("sample=%u level=%d edges=%u\n", sample++, digitalRead(4), edges);
  while (Serial.available()) {
    Serial.printf("rx=%02x\n", Serial.read());
  }
  delay(100);
}
```

The CLI script used the existing S3-compatible host-input mechanism:

```text
0.20 gpio 4 0
0.30 gpio 4 1
0.40 gpio 4 0
0.50 gpio 4 1
0.60 serial Z
```

SHA-256 inputs and temporary artifacts:

| Item | SHA-256 |
| --- | --- |
| `/tmp/esp32sim-classic-gpio-validation/platformio.ini` | `b86c69259be417474b2dfef705f80db5720beff2f80b471ef5762f2a65651500` |
| `/tmp/esp32sim-classic-gpio-validation/src/main.cpp` | `1c18cb54dc1929aafa5fefcbdbb3a89a96b8cec72562b9fc8f21d65ee1347593` |
| `firmware.factory.bin` | `d4838cbfd862e6b1bdb9a96d8d43c3e9ccc4960b510f5ed541cb5509b7653844` |
| `firmware.elf` | `f31527bca844cf90e1569c23870a1496203b7c64e740a8ac437cb232f9028101` |
| `/tmp/esp32-classic-gpio.script` | `37b2579dca5c9c2a9fb45050d5a0557082e558e694d569b120f31c8c493bf38f` |
| GPIO validation VCD | `c24e93dbfa30dadaf0a2e401ae45389512ca5dc0c4d24295f59ae8707348e077` |
| Blink validation VCD | `b7ef6b9fd286854bf82708e66d16f31f456e89077341ac4c23d2839e7e523e08` |

Build and run commands were:

```sh
cd /tmp/esp32sim-classic-gpio-validation
pio run

# From the repository root:
cargo build --release
target/release/esp32sim --chip esp32 --boot rom \
  --rom "$HOME/.platformio/packages/tool-esp-rom-elfs/esp32_rev300_rom.elf" \
  --flash-image /tmp/esp32sim-classic-gpio-validation/.pio/build/esp32dev/firmware.factory.bin \
  --elf /tmp/esp32sim-classic-gpio-validation/.pio/build/esp32dev/firmware.elf \
  --max-seconds 1.2 --no-reboot --no-dump \
  --script /tmp/esp32-classic-gpio.script \
  --vcd /tmp/esp32-classic-gpio-validation.vcd
```

The relevant UART output was:

```text
initial=1
sample=1 level=1 edges=0
[script] t=0.200s Gpio(4, false)
sample=2 level=0 edges=1
[script] t=0.300s Gpio(4, true)
sample=3 level=1 edges=1
[script] t=0.400s Gpio(4, false)
sample=4 level=0 edges=2
[script] t=0.500s Gpio(4, true)
sample=5 level=1 edges=2
[script] t=0.600s Serial("Z\n")
rx=5a
rx=0a
[emu] stop: Halted; emulated 1.200s (288000000 cycles)
```

The GPIO VCD contains the externally driven levels at exactly 0.2, 0.3, 0.4 and
0.5 modeled seconds:

```text
#200000000000 0s4
#300000000000 1s4
#400000000000 0s4
#500000000000 1s4
```

The unchanged blink artifact documented earlier was rerun for 2.2 modeled seconds. It
printed five `tick 0` lines and recorded GPIO2 changes at 0.518145, 1.018151, 1.518157
and 2.018163 seconds after its initial transition. This proves that `delay(500)` and the
FreeRTOS timer interrupt continue past two seconds. `millis()` still reports zero because
the classic high-resolution timer/latch path is not modeled; that limitation is retained.

At `d64ac6e`, these commands passed:

```sh
cargo test -p esp32
cargo test --workspace
cargo build --release
tools/wasm-build.sh
node tools/check-evidence-privacy.mjs
```

The workspace run includes the existing S3, C3 and C6 suites. The privacy checker
reported 1,472 tracked evidence files checked and no configured patterns found; manual
review found no retained user name, host name, device identifier or unrelated process
data in this extension.

## Timer and watchdog extension

Revision `7bcb5a9` extends EX199 again. The target, ROM and Arduino platform are the
same, but the mechanism and correctness contract differ from the boot and GPIO work:
TIMG0's classic 64-bit LACT must drive the ESP-IDF high-resolution timer, general TIMG
alarms must reach Arduino ISRs, and timer/RTC watchdog actions must reset the emulator
with a usable reset cause. This is functional modeled-time evidence, not a host-speed or
cycle-accuracy claim.

The installed Arduino-ESP32 source implements `micros()` and `millis()` with
`esp_timer_get_time()` in `cores/esp32/esp32-hal-misc.c`. Its ESP-IDF source selects
`PERIPH_TIMG0_MODULE` in `components/esp_timer/src/esp_timer_impl_lac.c`; the associated
`components/esp_hw_support/include/esp_private/systimer.h` selects LACT module 0 and two
LACT ticks per microsecond. The implementation therefore adds a classic-local TIMG
adapter rather than changing the shared S3/C3/C6 layout. It models LACT configuration,
divider, count latch, load, one-shot/auto-reload alarm and level/edge interrupt routing;
reuses the shared T0/T1 counter and alarm behavior at compatible offsets; and adds the
classic main-watchdog write protection, feed, stage actions and reset causes. The
existing RTC watchdog model is wired to the classic reset path. Reboot now preserves
the RTC reset-hint registers needed by ESP-IDF to translate a task-watchdog panic's
software CPU reset into `ESP_RST_TASK_WDT`.

The register-level LACT test was first run before the implementation and failed with
`left: 0`, `right: 10`: the old adapter treated classic offset `0x70` as the newer-chip
interrupt-enable register, so the counter never started. The same test then passed and
checks a latched count of 10, raw interrupt assertion, routed source assertion and
write-one-to-clear behavior. Other focused tests cover DPORT timer/watchdog delivery,
TIMG and RTC watchdog system-reset requests, reset-cause publication and RTC reset-hint
retention.

Both temporary validation projects used the same pinned `platformio.ini` shown above.
The first build attempt failed with `PermissionError: [Errno 1] Operation not permitted:
'$HOME/.platformio/platforms.lock'`; the one permitted retry against the existing
PlatformIO cache succeeded with `platform-espressif32` 55.3.38 and Arduino-ESP32 3.3.8.
No temporary build tree is retained in Git.

SHA-256 inputs and temporary artifacts:

| Item | SHA-256 |
| --- | --- |
| `/tmp/esp32sim-classic-timers-validation/platformio.ini` | `b86c69259be417474b2dfef705f80db5720beff2f80b471ef5762f2a65651500` |
| `/tmp/esp32sim-classic-timers-validation/src/main.cpp` | `87c67a9caec594dd75bb9b2e650d7948193deefa5e7b960ae5155b3d58b8eb0d` |
| timer `firmware.factory.bin` | `67bf709f709f72741a3ef83c06e16f1034ee9d55f88140c534c6b77495d3fb5e` |
| timer `firmware.elf` | `be51939601bac145a7c84889a45d4f8d038034e64c05dfa7ed78d07db4b30239` |
| `/tmp/esp32sim-classic-wdt-validation/platformio.ini` | `b86c69259be417474b2dfef705f80db5720beff2f80b471ef5762f2a65651500` |
| `/tmp/esp32sim-classic-wdt-validation/src/main.cpp` | `7601e4f2122c09eac0d74667cee1c9604013091734d88646687c0f30feb9303a` |
| watchdog `firmware.factory.bin` | `29d52770d0641a083fbb43a6ed5f1b03f5712d6884f9e3ef69e57c53c11a9a13` |
| watchdog `firmware.elf` | `1e4f7f3b1e1f1e23b469a332ad0b8e3ac93834ee8ef67543c7084b26798c67f2` |

The combined timer sketch was:

```cpp
#include <Arduino.h>
#include <esp_timer.h>

volatile uint32_t espTicks;
volatile uint32_t hwTicks;

void onEspTimer(void *) { ++espTicks; }
void ARDUINO_ISR_ATTR onHardwareTimer() { ++hwTicks; }

void setup() {
  Serial.begin(115200);
  esp_timer_create_args_t args = {};
  args.callback = onEspTimer;
  args.name = "periodic";
  esp_timer_handle_t periodic;
  ESP_ERROR_CHECK(esp_timer_create(&args, &periodic));
  ESP_ERROR_CHECK(esp_timer_start_periodic(periodic, 100000));
  hw_timer_t *hardware = timerBegin(1000000);
  timerAttachInterrupt(hardware, onHardwareTimer);
  timerAlarm(hardware, 50000, true, 0);
  Serial.printf("start millis=%lu micros=%lu\n", millis(), micros());
}

void loop() {
  delay(100);
  Serial.printf("sample millis=%lu micros=%lu esp=%u hw=%u\n",
                millis(), micros(), espTicks, hwTicks);
}
```

Build it with `pio run` in `/tmp/esp32sim-classic-timers-validation`. At committed
revision `7bcb5a9`, the exact run command was:

```sh
target/release/esp32sim --chip esp32 --boot rom \
  --rom "$HOME/.platformio/packages/tool-esp-rom-elfs/esp32_rev300_rom.elf" \
  --flash-image /tmp/esp32sim-classic-timers-validation/.pio/build/esp32dev/firmware.factory.bin \
  --elf /tmp/esp32sim-classic-timers-validation/.pio/build/esp32dev/firmware.elf \
  --max-seconds 0.8 --no-reboot --no-dump
```

The relevant serial output was:

```text
start millis=3 micros=3041
sample millis=102 micros=102525 esp=0 hw=1
sample millis=202 micros=202527 esp=1 hw=3
sample millis=302 micros=302528 esp=2 hw=5
sample millis=402 micros=402529 esp=3 hw=7
sample millis=502 micros=502530 esp=4 hw=9
sample millis=602 micros=602532 esp=5 hw=11
sample millis=702 micros=702533 esp=6 hw=13
[emu] stop: Halted; emulated 0.800s (192000000 cycles)
```

This checks that `micros()` follows modeled time, the 100 ms `esp_timer` callback keeps
firing after its initial scheduling latency, and the 50 ms hardware alarm continues at
twice that rate. The unchanged blink artifact was also rerun with:

```sh
target/release/esp32sim --chip esp32 --boot rom \
  --rom "$HOME/.platformio/packages/tool-esp-rom-elfs/esp32_rev300_rom.elf" \
  --flash-image /tmp/esp32sim-classic-pio/.pio/build/esp32dev/firmware.factory.bin \
  --elf /tmp/esp32sim-classic-pio/.pio/build/esp32dev/firmware.elf \
  --max-seconds 2.2 --no-reboot --no-dump
```

It now prints advancing values, superseding the retained earlier `tick 0` observation:

```text
CLASSIC_BOOT
tick 3
tick 502
tick 1002
tick 1502
tick 2002
[emu] stop: Halted; emulated 2.200s (528000000 cycles)
```

The task-watchdog sketch enables the Arduino loop watchdog and then stops feeding it:

```cpp
#include <Arduino.h>
#include <esp_system.h>

RTC_DATA_ATTR uint32_t boots;

void setup() {
  Serial.begin(115200);
  Serial.printf("boot=%u reset=%d\n", ++boots, esp_reset_reason());
  enableLoopWDT();
}

void loop() {
  Serial.println("starving loop watchdog");
  while (true) {}
}
```

Build it with `pio run` in `/tmp/esp32sim-classic-wdt-validation`, then run:

```sh
target/release/esp32sim --chip esp32 --boot rom \
  --rom "$HOME/.platformio/packages/tool-esp-rom-elfs/esp32_rev300_rom.elf" \
  --flash-image /tmp/esp32sim-classic-wdt-validation/.pio/build/esp32dev/firmware.factory.bin \
  --elf /tmp/esp32sim-classic-wdt-validation/.pio/build/esp32dev/firmware.elf \
  --max-seconds 6.2 --no-dump
```

The relevant output was:

```text
boot=1 reset=1
starving loop watchdog
E (10037) task_wdt: Task watchdog got triggered. The following tasks/users did not reset the watchdog in time:
E (10037) task_wdt:  - loopTask (CPU 1)
E (10037) task_wdt: Aborting.
[emu] chip reset at t=5.024s: cause 0xc (RTC_SW_CPU_RESET)
Rebooting...
rst:0xc (SW_CPU_RESET),boot:0x13 (SPI_FAST_FLASH_BOOT)
boot=1 reset=6
starving loop watchdog
[emu] stop: Halted; emulated 6.200s (1488000056 cycles)
```

The first diagnostic implementation rebooted at the same watchdog deadline but reported
`reset=3` after reboot. The timer reset recreated RTC register storage and erased the
ESP-IDF task-watchdog hint before `esp_reset_reason()` consumed it. Preserving the RTC
register storage and slow-clock state across reboot fixed the root cause; the rerun above
reports `6`, which is `ESP_RST_TASK_WDT`. The ROM's hardware cause remains the expected
panic path's software CPU reset (`0xc`).

Final checks at `7bcb5a9`:

```text
cargo test -p esp32       14 passed; 0 failed
cargo test --workspace    passed; external-firmware tests remained explicitly ignored
cargo build --release     passed
tools/wasm-build.sh       passed
```

The release and WASM builds cover the existing S3, C3 and C6 targets without changing
their shared timer code. `node tools/check-evidence-privacy.mjs` passed after this edit:
1,472 tracked evidence files and 15 gzip files checked, with no configured patterns
found. Manual review found no retained user name, host name, device identifier, raw
capture or unrelated process data in the timer extension.

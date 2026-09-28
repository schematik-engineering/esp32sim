# Project LED displays

`BoardModel::configure_led_displays` attaches at most 16 physical devices without
replacing existing circuit sensors, camera, strips, or displays. HT16K33 responds
on its configured SDA/SCL and 7-bit address on either hardware I2C controller.
TM1637 observes host GPIO output-enable/output changes; released lines are pulled
high by the module, and DIO is driven low only from the eighth falling clock edge
through the ninth falling edge for ACK. Its protocol is LSB first with START/STOP,
fixed/incrementing RAM writes, six RAM addresses, and display-control commands.

HT16K33 implements 16-byte display RAM, auto-increment/wrap and readback, oscillator
enable, display enable, 16 dimming levels, and nominal blink frequencies 2/1/0.5 Hz.
TM1637 dimming uses its specified duty ratios 1/2/4/10/11/12/13/14 over 16. Rendering
uses the Adafruit seven-segment, AlphaNum4, 8x8 mono and bicolor wiring. TM1637 has
four/six-digit decimal layouts or an explicitly selected four-digit center colon.
The RAM is retained across MCU reboot; the unchanged firmware driver initializes
it independently. New host configuration starts with deterministic zero RAM.

The WASM `esp32sim_led_displays(e, ptr, len)` call accepts eight-byte records before
boot: id, controller (1 HT16K33 / 2 TM1637), layout (1 seven / 2 alpha / 3 mono8x8 /
4 bicolor8x8), pin A (SDA/CLK), pin B (SCL/DIO), I2C address (TM uses zero), digit
count (HT uses four), flags (TM four-digit center colon is bit0). Other flags,
invalid identities/pins/layouts, duplicate routes, and I2C address collisions are
rejected before replacing the prior set.

Frames reuse binary RGB565 display format6 with IDs128..143 reserved for this
family. SPI display configuration rejects that reserved ID range. The adapter
maps these IDs back to project instance IDs. Seven-segment frames are
`digits*12+4` by20; alpha frames48x20; matrices8x8. The renderer is a visualization,
not an analog optical model. Keyscan inputs and LED scan electrical waveforms are
not modeled. No firmware patch or C++ facade is involved.

Protocol and wiring sources:

- [Holtek HT16K33 datasheet](https://cdn-shop.adafruit.com/datasheets/ht16K33v110.pdf)
- [Titan Micro TM1637 datasheet, hosted by M5Stack](https://m5stack.oss-cn-shenzhen.aliyuncs.com/resource/docs/datasheet/unit/digi_clock/TM1637.pdf)
- [Adafruit driver at c4ec132](https://github.com/adafruit/Adafruit_LED_Backpack/blob/c4ec1328fda9e4cbf355f6e407346347b4d207e2/Adafruit_LEDBackpack.cpp)
- [TM1637Display at 3cca196](https://github.com/avishorp/TM1637/blob/3cca19607013c49f6708b7f492765fea835431ca/TM1637Display.cpp)

# PSRAM command coverage

SPI1 CS1 now handles AP-compatible quad PSRAM ID (0x9f), reads
(0x03/0x0b/0xeb), and writes (0x02/0x38) against the allocated PSRAM.
The existing octal MR and read/write path remains in place. A zero-length
PSRAM allocation returns an absent-device response on either path.

The AP manufacturer/KGD bytes and 2/4/8 MiB density fields follow
[ESP-IDF's AP quad definitions](https://github.com/espressif/esp-idf/blob/v5.5.2/components/esp_psram/device/esp_quad_psram_defs_ap.h).
The real [initialization driver](https://github.com/espressif/esp-idf/blob/v5.5.2/components/esp_psram/device/esp_psram_impl_ap_quad.c)
first writes and reads a probe before inspecting the device ID. Previously
only octal commands were implemented, so an attached quad part failed this
probe and Arduino reported no PSRAM.

This models command data and capacity, with the existing immediate SPI
execution and memory mapping. Electrical timing, burst wrap, and quad-mode
entry/exit enforcement are not modeled. Quad IDs above 8 MiB are not invented.

The production adapter fixture in Schematik's `tests/fixtures/esp32sim/memory`
builds unchanged canonical RYMCU N8R2 and Seeed XIAO S3 firmware. It verifies
physical flash and PSRAM size, a patterned 1 MiB allocation, and firmware
reboot. Native tests also cover 4 MiB quad ID, absent quad/octal devices,
and RAM overwrites independent of flash erase semantics.

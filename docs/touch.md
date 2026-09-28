# Physical I2C touch input

Project touch controllers attach to the configured SDA/SCL pins. Firmware still programs the chip's I2C controller and GPIO matrix, reads controller registers, and handles the GPIO interrupt. The same I2C address can serve independent instances on different physical buses. Existing built-in board touch devices retain their original behavior.

## Host ABI

`esp32sim_configure_touches(e, bytes, length)` accepts up to 16 records of 12 bytes:

| Bytes | Value |
| --- | --- |
| 0 | Model: 1 GT911, 2 CST816S, 3 CST816T, 4 CST820, 5 AXS5106L |
| 1 | Host instance ID, 0..15 |
| 2, 3 | SDA, SCL GPIO |
| 4 | Seven-bit I2C address |
| 5, 6 | IRQ, reset GPIO; 255 means absent |
| 7 | AXS report rate, 1..120 Hz; 0 selects the simulator default of 120 Hz. Other models require 0. |
| 8..9, 10..11 | Physical glass width and height, little-endian u16, 1..4096 |

Configuration returns 0 on success, 1 for invalid wiring or conflicting identities/routes, and 2 for malformed bytes. It preserves other project devices. A zero-length configuration removes touch devices and releases their old IRQ inputs. Configure before running firmware; reconfiguration reattaches the board's I2C devices.

`esp32sim_touch_input(e,id,x,y,down)` accepts 0..4095 coordinates and down 0 or 1. Coordinates clamp to the physical glass. `esp32sim_touch_reset(e,id)` releases the source and clears its pending report. Both return 0 on success, 1 for an unknown instance, and 2 for invalid input. `esp32sim_touch_report_hz(e,id,hz)` updates an AXS instance without resetting its held input; it returns 0 on success and 1 for invalid input or another model.

Each source stores one current point and one pending release. A press followed by release before the driver reads the point remains readable once, followed by release. Movement replaces the current coordinates; this is not an event history. Firmware reboot keeps the physical source attached and held. Controller reset independently restores registers, wakes sleep, and samples the GT911 address strap. Host cleanup explicitly clears the source.

## Register and timing coverage

- GT911 supports product ID, point/status registers, explicit status acknowledgment, writable configuration including resolution, sleep command, and reset-time address selection between 0x5D and 0x14. Coordinates scale from the physical glass to the programmed resolution. Configuration requires a valid checksum and Config_Fresh. Gesture decoding, configuration persistence, and multiple contacts are not modeled. Edge IRQ polarity and level modes follow Module_Switch1. An edge IRQ stays asserted for the report period, regardless of acknowledgment, and repeats if unread or held. The separating idle edge lasts one emulated cycle; electrical rise/fall timing is not modeled.
- CST816S/T and CST820 use address 0x15 and distinct IDs B4/B5/B7. Point registers, sleep/reset, scan interval 0xEE, pulse width 0xED, and IRQ test/touch/change enables in 0xFA are modeled. Reading coordinates consumes the pending point but does not end the timed IRQ pulse. Gesture generation, automatic sleep, and the once-only gesture interrupt mode are not modeled.
- AXS5106L follows the current Waveshare driver at address 0x63, reading 14 bytes from register 0x01. There is no 0x3B alias. The manufacturer specifies a maximum report rate of 120 Hz but publishes no usable register timing table in the cited driver. The default 120 Hz cadence and 1 ms IRQ pulse are simulator assumptions. The host can calibrate cadence from 1 to 120 Hz. No fabricated chip ID or undocumented command protocol is exposed.

Timing advances in emulated CPU cycles. Host updates and calibration can occur while paused without executing firmware. Shared active-low IRQ lines combine by logical AND. The model does not simulate electrical contention or analog touch sensing.

## Sources

- [Goodix programming guide Rev.10, display vendor mirror](https://www.lcd-module.de/fileadmin/eng/pdf/zubehoer/GT911_Programming_Guide_Rev.10.pdf), configuration validation, IRQ timing and modes.
- [Goodix GT911 Arduino driver](https://github.com/tamctec/gt911-arduino/tree/b3f175e65a799368be9c544e255204e1e74ad2ed), register access, configuration, acknowledgment and address strap.
- [SensorLib CST816 driver](https://github.com/lewisxhe/SensorLib/tree/f3cc656a71ea9500f12ecbd4c4078ce9bd0282ea), controller IDs and point access.
- [Hynitron CST816S register description, vendor mirror](https://cdn.static.spotpear.com/uploads/picture/learn/common-lcd/lcd/1.28inch-round-lcd/CST816S%20Register%20Description-20190912.pdf), scan, pulse and IRQ control units.
- [Hynitron CST816S datasheet, Waveshare mirror](https://files.waveshare.com/wiki/common/CST816S_Datasheet_EN.pdf), reset and interrupt behavior.
- [Waveshare C6 Touch LCD 1.47 official examples](https://files.waveshare.com/wiki/ESP32-C6-Touch-LCD-1.47/ESP32-C6-Touch-LCD-1.47-Demo.zip), `Arduino/libraries/esp_lcd_touch_axs5106l`, address 0x63 and point packet. Archive SHA256 `ad8e27b172035fb73b5dbe88b821b1ff37bd677c20db294a9da7e5317ee176dd`.
- [ChipSourceTek AXS5106L product specification](https://en.chipsourcetek.com/Mcu-Chip/2416.html), maximum 120 Hz report rate.

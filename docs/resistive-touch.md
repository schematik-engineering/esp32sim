# Resistive touch controllers

Project XPT2046 and TSC2007 devices consume physical SPI or I2C traffic. Host coordinates represent a point on the attached glass; calibration converts that point to ADC samples. The firmware's original driver performs its own pressure test, filtering and rotation.

## Configuration and input

`esp32sim_configure_resistive_touches(e,ptr,len)` accepts 24-byte records. The combined capacitive/resistive limit is 16 instances with distinct IDs. Other project devices remain attached.

| Bytes | Meaning |
| --- | --- |
| 0, 1 | Model 1 XPT2046 or 2 TSC2007; instance ID 0..15 |
| 2..5 | XPT: SCLK, MOSI, MISO, CS GPIO. TSC: SDA, SCL, address, 255 |
| 6, 7 | IRQ GPIO or 255; reserved zero |
| 8..11 | Physical width, height, little-endian u16, 1..4096 |
| 12..23 | Six little-endian u16 ADC calibration values: X minimum, X maximum, Y minimum, Y maximum, Z1, Z2 |

The ADC values range from 0 to 4095. Each pair of axis endpoints must differ; reversed endpoints are valid. Generic simulator defaults are 0..4095 on both axes and Z1=1000, Z2=1800. These are adjustable assumptions, not a measured panel profile. TSC2007's actual Adafruit driver treats coordinate 4095 as no contact, so use panel calibration to keep active coordinates within the driver's range.

Configuration returns 0 on success, 1 for invalid physical routing/identity and 2 for malformed bytes. TSC addresses are 0x48..0x4B. SPI selection follows actual GPIO CS or the hardware CS route; disconnected MISO returns the bus's floating-high value while MOSI commands still reach the controller.

The existing `esp32sim_touch_input` and `esp32sim_touch_reset` functions address either touch family. `esp32sim_touch_calibrate(e,id,field,value)` changes one of the six calibration fields without executing CPU cycles or replacing the held source. It returns 0 on success, 1 for an unknown/non-resistive instance, and 2 for an invalid field/value. Invalid updates preserve the previous calibration.

A source retains one current point and one pending release. A short tap lasts through a complete coordinate acquisition; release follows the driver's power-down boundary. Firmware reboot keeps the external source and its calibration attached. Explicit host reset clears pending touch state.

## Supported transfers

XPT2046 supports byte-aligned 16/24-clock conversion sequences, pipelined X/Y/Z commands, 8/12-bit output, CS boundaries and PD0 pen-interrupt enable. It derives conversion progress from SPI clocks. PENIRQ is low during position/pressure conversion and re-enables after the result clocks complete. The existing SPI controller handles transfer format and routing.

TSC2007 supports X/Y/Z conversion commands, 8/12-bit output, address selection and power/IRQ control. The default filtered conversion takes approximately 100 microseconds at 12 bits or 50 at 8 bits, measured in emulated cycles. The Adafruit driver waits 500 microseconds. Reads before completion return the previous result; I2C clock stretching is not modeled. Reserved command functions NACK. Setup, driver activation and power-down commands are accepted, but analog noise/filter effects and pull-up resistance are not simulated.

Temperature, battery and auxiliary-input sensing are outside this touch slice. Their conversion result defaults to zero; the tested drivers only issue an ignored temperature conversion to restore power-down/IRQ state. There is no analog panel resistance network, contact noise, conversion droop or 15-clock SPI mode.

## Driver evidence

- [XPTEK XPT2046 datasheet, Waveshare mirror](https://files.waveshare.com/wiki/common/XPT2046_Datasheet.pdf), conversion commands, output pipeline and PENIRQ behavior.
- [Paul Stoffregen XPT2046 driver](https://github.com/PaulStoffregen/XPT2046_Touchscreen/tree/f956c5d8ce3bf39169c7378416b89e7cfe70a034), normal SPI transfers, filtering and rotation. With disconnected SPI input, this driver accepts floating-high data as a contact; the model preserves that behavior.
- [TI TSC2007 datasheet](https://www.ti.com/lit/ds/symlink/tsc2007.pdf), command map, resolution, address straps, conversion timing and IRQ control.
- [Adafruit TSC2007 driver](https://github.com/adafruit/Adafruit_TSC2007/tree/99a91e05758fd7aa440773562f9506e94655c788), normal I2C transfers and `getPoint().z`, which reports Z1. The former firmware facade returned Z2 minus Z1; the hardware driver determines the new result.

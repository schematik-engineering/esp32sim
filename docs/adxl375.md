# ADXL375

Sensor model 39 binds I²C GPIO routes at address 0x53 or 0x1d. Physical fields 4–6 are XYZ acceleration in m/s², bounded to ±1961.33. Defaults are 0, 0, 9.80665.

The model implements the ADXL375 Rev. B register map for bypass-mode measurements: DEVID, output data, BW_RATE, POWER_CTL, trim offsets and DATA_FORMAT justification. Signed counts use 49 mg/LSB; offsets add four counts per signed register unit. Conversion, startup and sleep exit use emulated clock time; standby retains the last sample. Sleep reduces rate and suppresses DATA_READY. Fresh sensor construction restores power-on state; CPU reboot leaves the external device intact.

`cargo test -p esp-soc adxl375` covers rate/startup timing, sign/scale, offset, formatting, sleep, standby, address validation, I²C snapshot reads, rejected modes and fresh-device reset.

Unsupported: SPI, FIFO, routed interrupt pins, shock/activity detection, autosleep, self-test and analog noise. Unsupported enabling writes return false. Threshold configuration written by the Adafruit initializer is stored without event detection.

Primary sources: https://www.analog.com/media/en/technical-documentation/data-sheets/ADXL375.pdf and https://github.com/adafruit/Adafruit_ADXL375/tree/20d259fc5bca4f5a500402e7abd2d45f702373de.

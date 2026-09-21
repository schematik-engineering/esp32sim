# External UART GPS

`esp32sim_gps_configure(emu, id, rx_pin, baud)` attaches an NMEA transmitter to a physical MCU RX GPIO. IDs are 0–3; baud rates are 1200–115200. Reconfiguring an ID replaces that transmitter. `esp32sim_gps_fix(emu, id, valid, lat, lng, altitude, speed, unix_ms)` sets or clears its fix. Both calls return zero on success and one for invalid configuration or values.

Coordinates are degrees, altitude metres, speed metres/second, and UTC milliseconds since the Unix epoch. Input timestamps cover 2000–2099. All numeric fix fields must be finite; latitude is within ±90, longitude ±180, altitude ±100000 and speed 0–10000. Setting `valid=0` emits invalid GGA/RMC sentences and discards pending old sentences. Parser retention of the previous valid location follows the firmware library's own semantics.

The shared machine scheduler emits GGA/RMC every emulated second, paced as ten bits per 8N1 byte. The UART receives a byte only when the firmware's GPIO matrix selects the connected pin and its programmed baud is within 3%. C6 uses PCR clock configuration; S3/C3 use UART clock configuration. Existing UART FIFO overflow and interrupt/timeout logic delivers bytes to the normal driver. Idle emulation stops at the next GPS byte deadline. Devices and their fix survive firmware resets, while resetting the browser worker creates fresh devices.

The pending queue holds one bounded pair of sentences per device. Slow baud rates delay the next pair rather than accumulate a backlog. There are no host parser callbacks or firmware symbol patches.

Verified with unchanged TinyGPSPlus v1.0.3 Arduino firmware on S3, C3 and C6 through the real WASM module. Scope excludes bit waveforms, SoftwareSerial, IO_MUX bypass paths, UBX configuration, I²C GNSS and satellite dynamics.

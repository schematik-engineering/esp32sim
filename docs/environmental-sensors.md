# Project I²C environmental sensors

`configure_circuit` kind 3 records are `[3,id,SDA,SCL,address,model,0,0]`:
model 1 BME280, model 2 BMP280, model 3 BH1750. There are at most 16 sensors.
Identity is per device; bus selection uses the actual GPIO matrix routes. Duplicate
sensor IDs or physical bus/address combinations are rejected. Existing camera
state is retained when configuring the circuit. External sensor state survives
firmware reboot; a fresh emulator starts a fresh sensor.

`esp32sim_sensor_set(emu,id,field,value)` returns 0 on acceptance, 1 otherwise:

| Field | Quantity | Accepted range |
|---|---|---|
| 0 | Temperature, Celsius | −40 to 85 |
| 1 | Relative humidity, percent | 0 to 100, BME280 only |
| 2 | Pressure, pascals | 30000 to 110000 |
| 3 | Light, lux | 0 to 120000, BH1750 only |

Nonfinite values and fields incompatible with the model are rejected.
`esp32sim_sensor_generation` returns 0 before measurement, MAX for an invalid ID,
and a changing counter for completed conversions. `esp32sim_sensor_value` returns
a latched compensated result, or NaN when invalid/skipped/unmeasured. Host updates
do not manufacture a reading. Values follow the raw register resolution and can
saturate at a gain-dependent limit.

## Conversion model

The Bosch devices expose read-only chip IDs and a fixed virtual factory trim.
Their nonlinear compensation equations are inverted by bounded binary search to
produce ADC samples. Firmware reads the trim and performs its ordinary
compensation. Register-encoding and real-driver tests cover packed humidity calibration.

Sleep, forced and normal modes, oversampling selection, skipped channels, normal
standby periods, conversion status, soft reset/NVM-copy delay and IIR filtering
are modeled. Burst reads snapshot the register set. Conversion state advances
using emulated CPU time, evaluated on access; skipped identical normal-mode
periods and filter decay are calculated in bounded time.

BH1750 implements power, reset, continuous/one-shot resolution commands, MTreg
gain/timing, two-byte big-endian output and automatic power-down. The model uses
typical 120 ms/16 ms conversion times scaled by MTreg. It has no optical noise or
spectral response model.

The register and timing references are the [Bosch BME280 data sheet](https://www.bosch-sensortec.com/media/boschsensortec/downloads/datasheets/bst-bme280-ds002.pdf),
[Bosch BMP280 data sheet](https://www.bosch-sensortec.com/media/boschsensortec/downloads/datasheets/bst-bmp280-ds001.pdf),
and [ROHM BH1750FVI technical note](https://www.mouser.com/datasheet/2/348/bh1750fvi-e-186247.pdf).
Electrical noise, analog transfer transients, SPI transport and other sensor
models are outside this implementation.

# MPU6050, INA219 and DS3231 register devices

These devices share the routed I²C interface and emulated CPU clock with the environmental sensors. A read transaction sees one register snapshot. Host input changes physical quantities; firmware configuration and conversion deadlines determine the latched registers. Sensor identities remain distinct across physical SDA/SCL routes. No firmware functions are intercepted.

Circuit kind 3 uses eight bytes: `[3,id,SDA,SCL,address,model,shuntLo,shuntHi]`. Models 4/5/6 select MPU6050/INA219/DS3231. Only INA219 accepts the last two bytes, an unsigned resistance in milliohms; zero defaults to 100. Other models require zero. Existing model IDs and input fields are unchanged.

| Model | Physical host inputs | Latched output fields |
|---|---|---|
| MPU6050, addresses 0x68/0x69 | 0 temperature −40..85°C; 4..6 acceleration XYZ ±156.9064m/s²; 7..9 angular velocity XYZ ±34.90658504rad/s | Same fields, after firmware-selected scaling and signed ADC clipping |
| INA219, addresses 0x40..0x4f | 10 bus voltage 0..32V; 11 shunt voltage ±320mV; 12 current in mA, bounded by ±320000/RmΩ; 15 integer shunt resistance 1..65535mΩ | 10 bus V, 11 shunt mV, 12 current mA, 13 power mW, 15 resistance mΩ |
| DS3231, address 0x68 | 0 temperature −40..85°C; 14 integer Unix UTC seconds from 2000-01-01 through 2099-12-31 | 0 temperature, 14 running UTC seconds |

INA current and shunt voltage are coupled by Ohm's law. Changing resistance preserves current and is rejected if the resulting voltage exceeds 320mV. Guest current/power register scaling follows its calibration register, not a host driver selection. An unchanged firmware driver that assumes 100mΩ reports half the current after the physical resistor changes to 50mΩ. The host measurement uses the configured physical resistor. The ADC can encode 32V; this is not a claim that physical INA219 hardware tolerates more than its documented 26V common-mode operating range.

MPU reset/sleep, sample divider, DLPF clock selection, range controls, data-ready status and signed acceleration/gyro/temperature registers are modeled. Defaults are 30°C, gravity on +Z, and zero angular velocity. This covers ordinary Adafruit and I2Cdevlib reads, not DMP, FIFO, self-test excitation, offset calibration, analog filtering, or the interrupt output pin. At rates above 1kHz, acceleration resampling is idealized alongside the gyro; the physical device repeats accelerometer samples.

INA conversion modes, typical conversion/averaging times, PGA clipping, calibration, current/power arithmetic, CNVR and math overflow are modeled. The input ADC is ideal and does not add noise or analog settling. Reverse current is signed. Reverse-current power register behavior is unverified: the model retains the signed arithmetic product's low 16 bits and omits the host power readout. Positive power and calibration are fixture-tested. This is a remaining compatibility limit, not a claimed reverse-power result.

DS date/time reads and writes use BCD, 12/24-hour format and leap years. Firmware time writes commit together at STOP. The default clock is 2000-01-01 with oscillator-stop status; an explicit host UTC sets the clock and clears that flag. Temperature updates after 200ms, then every 64s, or after a firmware-triggered conversion. Alarm interrupts, square-wave output, battery transitions and temperature-dependent oscillator drift are not modeled.

The unmodified RTClib 2.1.4 `getTemperature()` reads its signed temperature MSB as `uint8_t`. Thus bytes `FC C0` representing −3.25°C produce 252.75°C in that library. The model keeps the correct hardware bytes; the host result remains −3.25°C. The compatibility fixture records that library limitation.

## Sources

- [TDK/InvenSense MPU-6000/6050 register map, revision 4.0](https://cdn.sparkfun.com/datasheets/Sensors/Accelerometers/RM-MPU-6000A.pdf), manufacturer-authored archive: sample divider, configuration, sensor output and power-management registers.
- [Texas Instruments INA219 datasheet, revision G](https://www.ti.com/lit/ds/symlink/ina219.pdf): sections 8.5–8.6 define calibration equations, register layout, modes and conversion timing.
- [Analog Devices DS3231 datasheet](https://www.analog.com/media/en/technical-documentation/data-sheets/DS3231.pdf): timekeeping, control/status, temperature and I²C transfer behavior.

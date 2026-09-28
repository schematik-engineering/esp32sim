# I2C motion sensors

Circuit sensor models 15 (`lsm6ds3`, addresses 0x6a/0x6b) and 16 (`bno055`, 0x28/0x29) use the existing eight-byte sensor record. Bus pins, address and instance ID remain independent. They use the existing sensor input/value/generation ABI. SensorI2c's default address readiness and pointer increment hooks are unchanged for earlier devices; BNO055 uses timed address readiness and LSM6DS3 uses IF_INC.

Shared physical inputs: temperature field 0 in °C; acceleration 4–6 in m/s²; angular velocity 7–9 in rad/s. LSM6DS3 registers model WHO_AM_I=0x69, range scaling, independent acceleration/gyro ODR, 52 Hz temperature, data-ready clearing, byte order, BDU, IF_INC, power-down and the self-clearing software reset. Reset retains external physical inputs. DS33/DSOX are distinct chips and are not aliases.

BNO055 adds these fields:

| Fields | Meaning | Input |
| --- | --- | --- |
| 26–28 | Heading [0,360], roll [-90,90], pitch [-180,180], degrees | Yes |
| 29–31 | Magnetic X/Y/Z, µT [-1300,1300] | Yes |
| 32–35 | Quaternion W/X/Y/Z | No |
| 36–38 | Linear acceleration X/Y/Z, m/s² | No |
| 39–41 | Gravity X/Y/Z, m/s² | No |
| 42–45 | System/gyro/accel/mag calibration status, integer 0–3 | Yes, default 0 |

Pose is an ideal external physical input in Bosch's default Android axes, not a recreation of proprietary BSX fusion. The body-to-world matrix uses `Rz(-heading) Ry(-roll) Rx(-pitch)`; quaternion and gravity derive from that same rotation. Linear acceleration is total acceleration minus gravity. Firmware axis remapping transforms both vectors and pose; register units and Windows pitch convention are honored. Quaternion components are never independent host sliders. Calibration state is explicit; the model does not assert autonomous calibration.

BNO055 models page registers, documented IDs, 650 ms power/reset I2C unavailability, config mode, unit selection, suspend/resume, 100/20/50 Hz fusion rates, and a separate 20 Hz NDOF magnetic channel. External inputs survive chip reset and firmware reboot. The unchanged Adafruit driver's gyro vector uses degrees/s by default; host physical inputs/readouts remain rad/s.

Scope: direct I2C register measurements and ideal pose behavior. LSM FIFO, embedded pedometer/tap/sensor-hub/SPI/IRQ paths, BNO UART/HID/IRQ, analog noise, proprietary fusion dynamics, autonomous calibration and low-power motion detection are not covered. Non-fusion BNO raw output uses a nominal 100 Hz schedule; it does not reproduce every page-1 bandwidth/filter transient.

Primary evidence: [ST register driver](https://github.com/STMicroelectronics/lsm6ds3-pid), [ST datasheet mirrored by Arduino](https://docs.arduino.cc/resources/datasheets/LSM6DS3-datasheet.pdf), [ST reset guidance](https://community.st.com/mems-sensors-48/lsm6ds3-what-exactly-does-sw-reset-do-78552), [Bosch datasheet](https://www.bosch-sensortec.com/media/boschsensortec/downloads/datasheets/bst-bno055-ds000.pdf), [Bosch coordinate guide](https://www.bosch-sensortec.com/media/boschsensortec/downloads/application_notes_1/bst-bno055-an007.pdf).

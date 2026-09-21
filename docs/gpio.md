# GPIO input resolution

GPIO_IN now reads the resolved digital pin level. An explicitly supplied host level
has priority, followed by an enabled GPIO output latch, then the normal IO_MUX
pull-up/down configuration. A floating input or simultaneous pulls retain the
existing deterministic HIGH default. This is an ideal digital model; opposing
external and output drivers do not simulate electrical contention.

The ESP-IDF S3/C3/C6 `soc/io_mux_reg.h` and `hal/gpio_ll.h` define normal pull-down
as bit7 and pull-up as bit8, at IO_MUX offset `4 * (pin + 1)`. S3 pads22–25 do not
exist. The register writes retain their normal readback. Sleep/RTC pull selection,
USB PHY pulls and peripheral output waveforms are outside this change.

Effective transitions use the existing GPIO interrupt type and status registers.
S3 pulse counters observe the same transitions. Host drive presence and level
survive a firmware reboot, while output enables, latches and pulls reset.

`esp32sim_gpio_state` adds pull-up bit3 and pull-down bit4. Bit2 is the effective
input level; bits0/1 remain output enable/latch. The function is a level snapshot,
not a waveform interface.

# DHT and DS18B20 GPIO devices

The project board observes actual GPIO output and output-enable writes. Each sensor sinks its data line LOW or releases it; a release restores the MCU output and pull configuration. The sensor never supplies a simulated C++ driver result.

## DHT11 and DHT22

A host LOW of at least 18 ms for DHT11 or 1 ms for DHT22 starts a response. The device waits 20 us, acknowledges with 80 us LOW and 80 us HIGH, then sends forty bits. Each bit starts with 50 us LOW; HIGH lasts 27 us for zero or 70 us for one. The last byte is the modulo-256 sum of the other four bytes. DHT22 encodes tenths and a sign bit; DHT11 uses integer degrees and humidity. The device samples host environmental inputs when a valid start pulse arrives and reports a completed sample after the frame.

The model accepts at most one conversion every two seconds. DHT11 inputs are 0–50 °C and 20–90% RH; DHT22 inputs are −40–80 °C and 0–100% RH. Power-up stabilization, cable capacitance, analog voltage thresholds and temperature/humidity accuracy errors are not simulated.

Timing source: [Adafruit's DHT protocol explanation and datasheet timing](https://learn.adafruit.com/modern-replacements-for-dht11-dht22-sensors?view=all).

## DS18B20

Each device has a supplied eight-byte family-0x28 ROM, including its Dallas CRC. Several devices may share one wire and participate in the ROM search's real bit/complement/branch sequence. Match ROM isolates one device; Skip ROM broadcasts. Read ROM and scratchpad reads combine participating outputs as open-drain signals. An empty or wrong wire stays undriven.

The model recognizes the specified 480 us reset LOW with a 64-CPU-cycle tolerance at its lower boundary. This covers the measured integer-microsecond Arduino delay and emulator execution-horizon quantization; it is not an analog threshold accuracy claim. It waits 30 us, and emits 120 us presence LOW. Write slots distinguish a short LOW of at most 15 us from write-zero. Read-zero sinks the line until 60 us from the start of the read slot.

Convert T samples the physical temperature at its command. Completion occurs after the maximum specified conversion duration for the configured 9–12-bit resolution: 93.75, 187.5, 375 or 750 ms. The scratchpad starts at the documented 85 °C power-on value and changes only when conversion finishes. Resolution controls discarded low bits; CRC covers all eight scratchpad bytes. Copy Scratchpad takes 10 ms; Recall takes a modeled 1 ms, with busy polling and CRC update. Host physical temperature and saved alarm/configuration bytes survive an MCU-only reboot.

Devices use an external supply and answer Read Power Supply accordingly. Parasite power, strong-pullup voltage, alarm search, DS18S20/DS1822 variants and bus overdrive are outside this model. RTC-style clock drift and analog rise times are not inferred from ideal digital GPIO cycles.

Register and timing source: [Analog Devices DS18B20 datasheet](https://www.analog.com/media/en/technical-documentation/data-sheets/ds18b20.pdf).

## Configuration and bounds

`esp32sim_configure_pin_sensors` takes twelve-byte records `[model,id,pin,0,ROM x8]` before boot. Model IDs are 1 DHT11, 2 DHT22 and 3 DS18B20. There are at most sixteen devices, with unique IDs 0–15. DHT devices require zero ROM bytes and exclusive sensor pins. DS18B20 devices sharing a pin need distinct valid ROMs. GPIO sensors cannot share a data pin with configured touch or generic input protocols.

`esp32sim_pin_sensor_set` accepts field 0 in Celsius and field 1 in percent RH. DS18B20 accepts only temperature, −55–125 °C. Setters reject non-finite or out-of-range values. Completed-measurement generation and latched values are separate from host input state. Queues are bounded by the DHT forty-bit frame or one DS scratchpad/ROM transaction, and all deadlines use emulated CPU cycles.

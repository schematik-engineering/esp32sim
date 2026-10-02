use emu_core::Bus;
use esp_periph::{analog::AnalogStream, sar_adc, AnalogSource};
use esp_soc::{Soc, SocBus};
use std::{fs, path::PathBuf};

fn firmware<S: Soc>(mut m: esp_soc::Machine<S>, chip: &str, pin: u8) {
    let builds = PathBuf::from(
        std::env::var("ADC_PCM_FIRMWARE_DIR")
            .expect("set ADC_PCM_FIRMWARE_DIR to EX216 Arduino 3.3.8 .pio/build directory"),
    );
    let roms = PathBuf::from(
        std::env::var("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to Espressif ROM ELFs"),
    );
    let read = |path: PathBuf| {
        fs::read(&path).unwrap_or_else(|e| panic!("required ADC PCM input {}: {e}", path.display()))
    };
    m.console.capture = true;
    m.console.mask = 2;
    m.load_rom(&read(roms.join(format!(
        "esp32{chip}_rev{}_rom.elf",
        if chip == "c3" { 3 } else { 0 }
    ))))
    .unwrap();
    for (offset, name) in [
        (0, "bootloader.bin"),
        (0x8000, "partitions.bin"),
        (0x10000, "firmware.bin"),
    ] {
        m.write_flash(offset, &read(builds.join(chip).join(name)))
            .unwrap();
    }
    m.boot_rom();
    for _ in 0..1000 {
        m.run(100_000);
        if String::from_utf8_lossy(&m.console.uart0).contains("ADC READY") {
            break;
        }
    }
    assert!(String::from_utf8_lossy(&m.console.uart0).contains("ARDUINO=3.3.8 ADC READY"));
    for tone in [false, true] {
        let start = m.bus.cycles();
        let stream = AnalogStream::new(16000, S::CPU_HZ, 1.0, 0.4, start).unwrap();
        m.bus.analog_set(pin, AnalogSource::Stream(stream.clone()));
        m.console.uart0.clear();
        m.bus.uart_input(0, b"R");
        let mut pushed = 0u64;
        for _ in 0..2000 {
            if tone {
                let now = m.bus.cycles();
                let target = (now - start) * 16000 / S::CPU_HZ + 512;
                let samples: Vec<_> = (pushed..target)
                    .map(|n| {
                        (16384.0 * (std::f64::consts::TAU * 440.0 * n as f64 / 16000.0).sin())
                            .round() as i16
                    })
                    .collect();
                stream.push(&samples, now);
                pushed = target;
            }
            m.run(100_000);
            if String::from_utf8_lossy(&m.console.uart0).contains("ADC DONE") {
                break;
            }
        }
        let output = String::from_utf8_lossy(&m.console.uart0);
        assert!(output.contains("ADC DONE"), "{chip}: {output}");
        let samples: Vec<(f64, f64)> = output
            .lines()
            .filter_map(|line| {
                let mut fields = line.strip_prefix("SAMPLE ")?.split_whitespace();
                let time = fields.next()?.parse::<f64>().unwrap() / 1e6;
                let raw = fields.next()?.parse::<u32>().unwrap();
                let mv = match chip {
                    "s3" => esp_periph::rtc_cntl::idf44_s3_adc1::millivolts(raw, 3) as f64,
                    "c3" => sar_adc::millivolts(raw, 3, sar_adc::Calibration::C3) as f64,
                    _ => sar_adc::millivolts(raw, 3, sar_adc::Calibration::C6) as f64,
                };
                Some((time, mv))
            })
            .collect();
        assert_eq!(samples.len(), 1024);
        let mean = samples.iter().map(|s| s.1).sum::<f64>() / samples.len() as f64;
        let rate = 1023.0 / (samples[1023].0 - samples[0].0);
        assert!((rate - 8000.0).abs() < 160.0, "{chip} sample rate {rate}");
        let amplitude = |frequency: f64| {
            let (re, im) = samples.iter().fold((0.0, 0.0), |(re, im), &(t, v)| {
                let phase = std::f64::consts::TAU * frequency * (t - samples[0].0);
                (re + (v - mean) * phase.cos(), im + (v - mean) * phase.sin())
            });
            re.hypot(im) * 2.0 / samples.len() as f64
        };
        let (frequency, peak) = (1..4000)
            .map(|f| (f, amplitude(f as f64)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        if tone {
            assert!(
                (frequency - 440i32).abs() <= 5,
                "{chip} frequency {frequency}"
            );
            assert!((peak - 200.0).abs() <= 10.0, "{chip} peak amplitude {peak}");
        } else {
            assert!(samples.iter().all(|s| s.1 == mean));
            assert!((mean - 1000.0).abs() <= 2.0);
        }
        let now = m.bus.cycles();
        let before = m.bus.adc_observation(pin).unwrap().generation;
        stream.push(&vec![123; 48000], now);
        assert_eq!(stream.queued_samples(now), 32000);
        for _ in 0..10 {
            m.run(100_000);
        }
        assert_eq!(m.bus.adc_observation(pin).unwrap().generation, before);
        stream.push(&vec![456; 48000], m.bus.cycles());
        assert_eq!(stream.queued_samples(m.bus.cycles()), 32000);
        println!("{chip} tone={tone} samples={} rate_hz={rate:.3} dominant_hz={frequency} amplitude_mv={peak:.3} mean_mv={mean:.3} pushed={pushed} stopped_queue=32000 insns={} cycles={}", samples.len(), m.insns(), m.bus.cycles());
    }
}

#[test]
#[ignore = "requires local Arduino 3.3.8 firmware; see EX216"]
fn external_adc_pcm_s3() {
    firmware(esp32s3::machine([0; 6]), "s3", 1);
}
#[test]
#[ignore = "requires local Arduino 3.3.8 firmware; see EX216"]
fn external_adc_pcm_c3() {
    firmware(esp32c3::machine([0; 6], 4 << 20), "c3", 0);
}
#[test]
#[ignore = "requires local Arduino 3.3.8 firmware; see EX216"]
fn external_adc_pcm_c6() {
    firmware(esp32c6::machine([0; 6], 8 << 20), "c6", 0);
}

fn conversions<S: Soc>(mut m: esp_soc::Machine<S>, pin: u8, control: u32, select: u32, start: u32) {
    let stream = AnalogStream::new(8000, S::CPU_HZ, 0.5, 0.5, 0).unwrap();
    stream.push(&[16384, -16384, 0], 0);
    m.bus.analog_set(pin, AnalogSource::Stream(stream.clone()));
    let mut readings = Vec::new();
    for sample in 0..4 {
        if sample == 2 {
            m.bus.reboot([0; 6]);
        }
        m.bus.tick((S::CPU_HZ / 8000) as u32);
        m.bus.write32(control, select).unwrap();
        m.bus.write32(control, select | start).unwrap();
        let observation = m.bus.adc_observation(pin).unwrap();
        assert_eq!(observation.generation, sample + 1);
        readings.push(observation.raw);
    }
    assert!(
        readings[0] > readings[2] && readings[2] > readings[1],
        "{readings:?}"
    );
    assert_eq!(readings[2], readings[3]);
    assert_eq!(stream.queued_samples(m.bus.cycles()), 0);
}

#[test]
fn stream_conversions_follow_time_across_reset() {
    conversions(esp32s3::machine([0; 6]), 1, 0x6000880c, 1 << 19, 1 << 17);
    conversions(
        esp32c3::machine([0; 6], 4 << 20),
        0,
        0x60040020,
        1 << 31,
        1 << 29,
    );
    conversions(
        esp32c6::machine([0; 6], 8 << 20),
        0,
        0x6000e020,
        1 << 31,
        1 << 29,
    );
}

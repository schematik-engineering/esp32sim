//! Developer-only check of the unchanged EX208 Arduino-ESP32 3.3.8 ESP_I2S sketch.
use emu_core::Bus;
use esp_periph::i2s::{PcmPins, PcmSource};
use esp_soc::{Soc, SocBus};
use std::{fs, path::PathBuf};

fn firmware<S: Soc>(make: impl Fn() -> esp_soc::Machine<S>, env: &str) {
    let builds = PathBuf::from(
        std::env::var("PCM_FIRMWARE_DIR")
            .expect("set PCM_FIRMWARE_DIR to EX208 Arduino 3.3.8 .pio/build directory"),
    );
    let roms = PathBuf::from(
        std::env::var("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to Espressif ROM ELFs"),
    );
    let chip = env.split('-').next().unwrap();
    let read = |path: PathBuf| {
        fs::read(&path)
            .unwrap_or_else(|e| panic!("required PCM firmware input {}: {e}", path.display()))
    };
    for wired in [Some(0), Some(15), None] {
        let mut m = make();
        m.console.capture = true;
        m.console.mask = 2;
        m.load_rom(&read(roms.join(format!(
            "esp32{chip}_rev{}_rom.elf",
            if chip == "c3" { 3 } else { 0 }
        ))))
        .unwrap();
        let build = builds.join(env);
        m.write_flash(0, &read(build.join("bootloader.bin")))
            .unwrap();
        m.write_flash(0x8000, &read(build.join("partitions.bin")))
            .unwrap();
        m.write_flash(0x10000, &read(build.join("firmware.bin")))
            .unwrap();
        for (id, frequency) in [(0, 1000.0), (15, 2000.0)] {
            let pins = if wired == Some(id) {
                [4, 5, 6]
            } else {
                [7, 8, 9]
            };
            let pins = if env.ends_with("pdm") {
                PcmPins::Pdm {
                    clk: pins[0],
                    data: pins[2],
                }
            } else {
                PcmPins::I2s {
                    bclk: pins[0],
                    ws: pins[1],
                    data: pins[2],
                }
            };
            let mut source = PcmSource::new(16000, pins).unwrap();
            let tone: Vec<_> = (0..48000)
                .map(|n| {
                    let sample = (32767.0
                        * 0.5
                        * (std::f64::consts::TAU * frequency * f64::from(n) / 16000.0).sin())
                    .round() as i16;
                    [sample; 2]
                })
                .collect();
            source.push(&tone);
            assert_eq!(source.queued_frames(), 32000);
            m.bus.pcm_sources().unwrap().inputs[id] = Some(source);
        }
        m.boot_rom();
        let mut resets = 0;
        let mut observed = None;
        let mut readings = 0;
        for _ in 0..10000 {
            let stop = m.run(100_000);
            if let Some(id) = m.bus.i2s_selected_source(0) {
                observed = Some(id);
            }
            let output = String::from_utf8_lossy(&m.console.uart0);
            readings = output
                .lines()
                .filter(|line| line.starts_with("RX_BYTES="))
                .count();
            if resets > 0 && readings >= 2 {
                break;
            }
            if matches!(stop, esp_soc::Stop::SwReset) {
                m.reboot();
                resets += 1;
            }
        }
        let output = String::from_utf8_lossy(&m.console.uart0);
        assert!(output.contains("ARDUINO=3.3.8"), "{output}");
        assert!(!output.contains("RX_BEGIN=0"), "{output}");
        assert!(
            resets > 0 && readings >= 2,
            "{env} missing post-reset read: {output}"
        );
        assert_eq!(observed, wired);
        for line in output.lines().filter(|line| line.starts_with("RX_BYTES=")) {
            let values: Vec<f64> = line
                .split_whitespace()
                .map(|field| field.split_once('=').unwrap().1.parse().unwrap())
                .collect();
            assert_eq!(values[0], 512.0);
            if let Some(id) = wired {
                let expected = 32767.0 * 0.5 / 2.0_f64.sqrt();
                assert!(
                    (values[1] - expected).abs() / expected < 0.01,
                    "{env} {line}"
                );
                assert!(
                    (values[2] - if id == 0 { 1000.0 } else { 2000.0 }).abs() <= 62.5,
                    "{env} {line}"
                );
            } else {
                assert_eq!(&values[1..], &[0.0, 0.0]);
            }
            println!("{env} wired={wired:?} {line}");
        }
        for id in [0, 15] {
            let consumed = m.bus.pcm_sources().unwrap().inputs[id]
                .as_ref()
                .unwrap()
                .consumed_frames;
            assert_eq!(consumed > 0, wired == Some(id));
            println!("{env} wired={wired:?} source={id} consumed={consumed}");
        }
        // The unchanged sketch calls mic.end(), then delays before reset.
        let i2s = match chip {
            "s3" => 0x6000f000,
            "c3" => 0x6002d000,
            _ => 0x6000c000,
        };
        for _ in 0..100 {
            if m.bus.read32(i2s + 0x20).unwrap() & 4 == 0 {
                break;
            }
            m.run(100_000);
        }
        assert_eq!(m.bus.read32(i2s + 0x20).unwrap() & 4, 0);
        let source = m.bus.pcm_sources().unwrap().inputs[0].as_mut().unwrap();
        let consumed = source.consumed_frames;
        source.push(&vec![[999; 2]; 48000]);
        assert_eq!(source.queued_frames(), 32000);
        let before = m.bus.cycles();
        let mut stopped_at_reset = false;
        for _ in 0..1000 {
            if matches!(m.run(100_000), esp_soc::Stop::SwReset) {
                stopped_at_reset = true;
                break;
            }
        }
        assert!(stopped_at_reset);
        let source = m.bus.pcm_sources().unwrap().inputs[0].as_ref().unwrap();
        assert!(source.queued_frames() < 32000);
        assert_eq!(source.consumed_frames, consumed);
        let queued = source.queued_frames();
        println!(
            "{env} wired={wired:?} stopped_rx_queue={queued} capacity=32000 elapsed_cycles={}",
            m.bus.cycles() - before
        );
        println!(
            "{env} wired={wired:?} resets={resets} insns={} cycles={}",
            m.insns(),
            m.bus.cycles()
        );
    }
}

#[test]
#[ignore = "requires local Arduino 3.3.8 builds; see EX213"]
fn external_pcm_arduino_s3() {
    firmware(|| esp32s3::machine([0; 6]), "s3");
}
#[test]
#[ignore = "requires local Arduino 3.3.8 builds; see EX213"]
fn external_pcm_arduino_c3() {
    firmware(|| esp32c3::machine([0; 6], 4 << 20), "c3");
}
#[test]
#[ignore = "requires local Arduino 3.3.8 builds; see EX213"]
fn external_pcm_arduino_c6() {
    firmware(|| esp32c6::machine([0; 6], 8 << 20), "c6");
}
#[test]
#[ignore = "requires local Arduino 3.3.8 builds; see EX213"]
fn external_pcm_arduino_s3_pdm() {
    firmware(|| esp32s3::machine([0; 6]), "s3-pdm");
}

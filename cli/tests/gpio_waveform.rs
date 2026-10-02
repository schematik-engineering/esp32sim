use emu_core::{Bus, Core};
use esp_soc::{board::BoardModel, devices::Ws2812Chain, Machine, Soc, SocBus};
use std::sync::{Arc, Mutex};

struct Capture {
    strip: Ws2812Chain,
    edges: Vec<(u64, bool)>,
    raw_edges: Vec<(u64, bool)>,
}
struct Board {
    state: Arc<Mutex<Capture>>,
    hz: u32,
    pin: u8,
}
impl BoardModel for Board {
    fn name(&self) -> &'static str {
        "waveform-test"
    }
    fn gpio_output_at(&mut self, cycle: u64, _: &[(u8, bool)], enabled: u64, output: u64) {
        if enabled & (1u64 << self.pin) == 0 {
            return;
        }
        let high = output & (1u64 << self.pin) != 0;
        let mut s = self.state.lock().unwrap();
        if s.raw_edges.last().is_none_or(|&(_, last)| last != high) {
            s.raw_edges.push((cycle, high));
        }
    }
    fn gpio_waveform_at(
        &mut self,
        cycle: u64,
        gpio: &esp_periph::gpio::Gpio,
        mux: &esp_periph::RegRam,
        signal: u32,
    ) {
        let level = gpio.software_output(self.pin, mux, signal);
        let mut s = self.state.lock().unwrap();
        if let Some(high) = level {
            if s.edges.last().is_none_or(|&(_, last)| last != high) {
                s.edges.push((cycle, high));
            }
        }
        s.strip
            .gpio_drive(cycle, self.hz, level.is_some(), level.unwrap_or(false));
    }
    fn advance_to(&mut self, cycle: u64) {
        self.state
            .lock()
            .unwrap()
            .strip
            .advance_gpio(cycle, self.hz);
    }
}
fn board(hz: u32, pin: u8) -> (Box<dyn BoardModel>, Arc<Mutex<Capture>>) {
    let state = Arc::new(Mutex::new(Capture {
        strip: Ws2812Chain::new(3),
        edges: Vec::new(),
        raw_edges: Vec::new(),
    }));
    (
        Box::new(Board {
            state: state.clone(),
            hz,
            pin,
        }),
        state,
    )
}

fn pulse_program(xtensa: bool, hz: u64) -> Vec<u8> {
    let mut code = Vec::new();
    for byte in [11u8, 7, 13, 19, 17, 23, 31, 29, 37] {
        for bit in (0..8).rev() {
            for (high, ns) in [
                (true, if byte & (1 << bit) != 0 { 800 } else { 400 }),
                (false, 800),
            ] {
                if xtensa {
                    code.extend([0x22, 0x61, if high { 0 } else { 1 }]);
                } else {
                    code.extend((0x0020_a023u32 | if high { 0 } else { 4 << 7 }).to_le_bytes());
                }
                for _ in 1..ns * hz / 1_000_000_000 {
                    if xtensa {
                        code.extend([0x3d, 0xf0]);
                    } else {
                        code.extend([1, 0]);
                    }
                }
            }
        }
    }
    if xtensa {
        code.extend([0x06, 0xff, 0xff]);
    } else {
        code.extend(0x0000_006fu32.to_le_bytes());
    }
    code
}
fn check<S: Soc>(
    mut m: Machine<S>,
    state: Arc<Mutex<Capture>>,
    code: Vec<u8>,
    routes: (u32, u32, u32),
    quantum: u64,
    core: usize,
) -> Machine<S> {
    let (gpio, mux, signal) = routes;
    m.quantum = quantum;
    m.console.capture = true;
    m.bus.write32(mux + 4 + 4 * 4, 1 << 12).unwrap();
    m.bus.write32(gpio + 0x554 + 4 * 4, signal).unwrap();
    m.bus.write32(gpio + 0x24, 1 << 4).unwrap();
    let pc = if gpio == 0x6009_1000 {
        0x4080_0000
    } else {
        0x4038_0000
    };
    m.bus.load_bytes(pc, &code).unwrap();
    m.cores[core].set_pc(pc);
    m.max_cycles = 100_000;
    m.run(100_000);
    let s = state.lock().unwrap();
    assert_eq!(
        s.strip.leds,
        [[7, 11, 13], [17, 19, 23], [29, 31, 37]],
        "q={quantum} edges={:?}",
        &s.edges[..s.edges.len().min(8)]
    );
    assert_eq!(s.edges.len(), 145);
    assert_eq!(
        s.raw_edges, s.edges,
        "legacy callback keeps exact timestamps"
    );
    assert!(s.edges.windows(2).all(|e| e[0].0 <= e[1].0));
    let widths: Vec<_> = s.edges[1..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|e| e[1].0 - e[0].0)
        .collect();
    assert!(widths
        .iter()
        .all(|&w| w == S::CPU_HZ * 400 / 1_000_000_000 || w == S::CPU_HZ * 800 / 1_000_000_000));
    m
}
#[test]
fn instruction_timestamps_decode_on_all_chips_and_quantum_sizes() {
    for q in [1, 64, 256, 1024] {
        for jit in [false, true] {
            for core in [0, 1] {
                let mut m = esp32s3::machine([0; 6]);
                if core == 1 {
                    m.bus.load_bytes(0x4037_0000, &[0x06, 0xff, 0xff]).unwrap();
                    m.cores[0].pc = 0x4037_0000;
                    m.bus.write32(0x600c_0000, 2).unwrap();
                    m.run(64);
                    m.cores[0].waiting = true;
                }
                let (b, s) = board(240_000_000, 4);
                m.bus.board = b;
                m.cores[core].set_jit(jit);
                m.cores[core].ps = 0;
                m.cores[core].set_ar(1, 0x6000_4008);
                m.cores[core].set_ar(2, 1 << 4);
                let m = check(
                    m,
                    s,
                    pulse_program(true, 240_000_000),
                    (0x6000_4000, 0x6000_9000, 256),
                    q,
                    core,
                );
                if jit && q >= 64 {
                    assert!(m.cores[core].blocks.jit_instructions > 0);
                }
            }
        }
        let mut m = esp32c3::machine([0; 6], 4 << 20);
        let (b, s) = board(160_000_000, 4);
        m.bus.board = b;
        m.cores[0].x[1] = 0x6000_4008;
        m.cores[0].x[2] = 1 << 4;
        check(
            m,
            s,
            pulse_program(false, 160_000_000),
            (0x6000_4000, 0x6000_9000, 128),
            q,
            0,
        );
        let mut m = esp32c6::machine([0; 6], 4 << 20);
        let (b, s) = board(160_000_000, 4);
        m.bus.board = b;
        m.cores[0].x[1] = 0x6009_1008;
        m.cores[0].x[2] = 1 << 4;
        check(
            m,
            s,
            pulse_program(false, 160_000_000),
            (0x6009_1000, 0x6009_0000, 128),
            q,
            0,
        );
    }
}

fn firmware<S: Soc>(
    mut m: Machine<S>,
    state: Arc<Mutex<Capture>>,
    chip: &str,
    case: &str,
    command: &str,
) {
    let root = std::path::PathBuf::from(std::env::var_os("ESP32SIM_WAVEFORM_DIR").expect(
        "set ESP32SIM_WAVEFORM_DIR to the three-chip firmware directory described in EX215",
    ));
    let roms = std::path::PathBuf::from(
        std::env::var_os("ESP32SIM_ROM_DIR")
            .expect("set ESP32SIM_ROM_DIR to the absolute ROM directory"),
    );
    let rev = if chip == "c3" { 3 } else { 0 };
    m.load_rom(&std::fs::read(roms.join(format!("esp32{chip}_rev{rev}_rom.elf"))).unwrap())
        .unwrap();
    m.write_flash(
        0,
        &std::fs::read(root.join(case).join("firmware.factory.bin")).unwrap(),
    )
    .unwrap();
    m.boot_rom();
    m.console.capture = true;
    m.console.mask = 2;
    if !command.is_empty() {
        m.load_script(&format!("0.1 uart0 {command}")).unwrap();
    }
    m.max_cycles = S::CPU_HZ * 2;
    m.run(u64::MAX);
    let text = String::from_utf8_lossy(&m.console.all);
    println!("{chip}: {text}");
    let s = state.lock().unwrap();
    println!(
        "{chip}: cycles={} instructions={} edges={} colours={:?}",
        m.bus.cycles(),
        m.insns(),
        s.edges.len(),
        s.strip.leds
    );
    println!(
        "{chip}: first_edges={:?}",
        &s.edges[..s.edges.len().min(12)]
    );
    let marker = if command.is_empty() {
        "WAVEFORM DONE".to_string()
    } else {
        format!("OUTPUT_{command}:DONE")
    };
    assert!(text.contains(&marker));
    let want = if command == "A" {
        [[255, 0, 0], [0, 255, 0], [0, 0, 255]]
    } else {
        [[7, 11, 13], [17, 19, 23], [29, 31, 37]]
    };
    assert_eq!(s.strip.leds, want);
}
#[test]
#[ignore = "requires local Arduino-ESP32 3.3.8 firmware; see EX215"]
fn external_arduino_ws2812_all_chips() {
    let mut m = esp32s3::machine([0; 6]);
    let (b, s) = board(240_000_000, 4);
    m.bus.board = b;
    firmware(m, s, "s3", "s3", "");
    let mut m = esp32c3::machine([0; 6], 4 << 20);
    let (b, s) = board(160_000_000, 4);
    m.bus.board = b;
    firmware(m, s, "c3", "c3", "");
    let mut m = esp32c6::machine([0; 6], 4 << 20);
    let (b, s) = board(160_000_000, 4);
    m.bus.board = b;
    firmware(m, s, "c6", "c6", "");
}

#[test]
fn c6_reset_matrix_does_not_invert_software_gpio() {
    let mut m = esp32c6::machine([0; 6], 4 << 20);
    for reset in [false, true] {
        if reset {
            m.bus.reboot([0; 6]);
        }
        assert_eq!(m.bus.read32(0x6009_1564).unwrap(), 128);
        m.bus.write32(0x6009_0014, 1 << 12).unwrap();
        m.bus.write32(0x6009_1024, 1 << 4).unwrap();
        for (route, want) in [
            (128, Some(false)),
            (128 | 256, Some(true)),
            (128 | 1024, None),
            (3, None),
        ] {
            m.bus.write32(0x6009_1564, route).unwrap();
            assert_eq!(
                m.bus
                    .periph
                    .gpio
                    .software_output(4, &m.bus.periph.io_mux, 128),
                want
            );
        }
    }
}

#[test]
#[ignore = "requires original NeoPixelBus fixture flash images; see EX215"]
fn external_neopixelbus_c3_c6() {
    for (pin, command) in [(0, "A"), (1, "E")] {
        let mut m = esp32c3::machine([0; 6], 4 << 20);
        let (b, s) = board(160_000_000, pin);
        m.bus.board = b;
        firmware(m, s, "c3", "c3-neopixel", command);
        let mut m = esp32c6::machine([0; 6], 4 << 20);
        let (b, s) = board(160_000_000, pin);
        m.bus.board = b;
        firmware(m, s, "c6", "c6-neopixel", command);
    }
}

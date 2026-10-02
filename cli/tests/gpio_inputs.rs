use emu_core::Bus;
use esp_soc::{board::BoardEdge, BoardModel, SocBus};
use std::{cell::RefCell, rc::Rc};

struct Sonar {
    hz: u64,
    trigger: u8,
    echo: u8,
    high: Option<u64>,
    edges: Vec<BoardEdge>,
    due: Vec<BoardEdge>,
    widths: Rc<RefCell<Vec<u64>>>,
}
impl BoardModel for Sonar {
    fn name(&self) -> &'static str {
        "sonar"
    }
    fn input_levels(&self) -> Vec<(u8, bool)> {
        vec![(self.echo, false)]
    }
    fn gpio_output_at(&mut self, cycle: u64, _: &[(u8, bool)], enable: u64, out: u64) {
        let high = enable & out & (1 << self.trigger) != 0;
        if high && self.high.is_none() {
            self.high = Some(cycle);
        }
        if !high {
            if let Some(start) = self.high.take() {
                let width = cycle - start;
                self.widths.borrow_mut().push(width);
                if width >= self.hz / 100_000 {
                    self.edges = vec![
                        BoardEdge {
                            cycle: cycle + self.hz / 10_000,
                            pin: self.echo,
                            level: true,
                        },
                        BoardEdge {
                            cycle: cycle + self.hz * 5900 / 1_000_000,
                            pin: self.echo,
                            level: false,
                        },
                    ];
                }
            }
        }
    }
    fn next_deadline(&self) -> Option<u64> {
        self.edges.first().map(|e| e.cycle)
    }
    fn advance_to(&mut self, cycle: u64) {
        while self.edges.first().is_some_and(|e| e.cycle <= cycle) {
            self.due.push(self.edges.remove(0));
        }
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> {
        std::mem::take(&mut self.due)
    }
}

macro_rules! firmware {
    ($name:ident, $ctor:expr, $chip:literal, $hz:expr, $trigger:expr, $echo:expr, $rom:literal) => {
        #[test]
        #[ignore = "requires retained Arduino 3.3.8 input firmware; set GPIO_INPUT_FIRMWARE"]
        fn $name() {
            use std::{env, fs, path::PathBuf};
            let root = PathBuf::from(
                env::var_os("GPIO_INPUT_FIRMWARE")
                    .expect("set GPIO_INPUT_FIRMWARE to extracted input fixture binaries"),
            );
            let dir = root.join($chip);
            let rom = PathBuf::from(
                env::var_os("ESP32SIM_ROM_DIR")
                    .expect("set ESP32SIM_ROM_DIR to mask ROM directory"),
            );
            let mut m = $ctor;
            m.bus.set_flash_size(8 * 1024 * 1024);
            m.quantum = env::var("GPIO_QUANTUM")
                .ok()
                .map(|s| s.parse().unwrap())
                .unwrap_or(1);
            if std::env::var_os("GPIO_APPROXIMATE").is_some() {
                m.set_cost_model(Box::<esp32s3::ApproximateCostModel>::default())
                    .unwrap();
            }
            m.console.capture = true;
            let widths = Rc::new(RefCell::new(Vec::new()));
            m.bus.board = Box::new(Sonar {
                hz: $hz,
                trigger: $trigger,
                echo: $echo,
                high: None,
                edges: vec![],
                due: vec![],
                widths: widths.clone(),
            });
            m.bus.attach_board_devices();
            m.load_rom(&fs::read(rom.join($rom)).unwrap()).unwrap();
            for offset in fs::read_to_string(dir.join("flash.txt")).unwrap().lines() {
                m.write_flash(
                    offset.parse().unwrap(),
                    &fs::read(dir.join(offset)).unwrap(),
                )
                .unwrap();
            }
            m.boot_rom();
            let mut phase = 0;
            while m.seconds() < 3.0 {
                assert!(matches!(m.run(500_000), esp_soc::Stop::MaxInsns));
                let serial = String::from_utf8_lossy(&m.console.uart0).into_owned();
                if phase == 0 && serial.contains("INPUT:READY") {
                    m.bus.uart_input(0, b"U");
                    phase = 1;
                }
                if phase == 1 && serial.contains("INPUT:ULTRASONIC") {
                    m.bus.uart_input(0, b"P");
                    phase = 2;
                }
                if let Some(result) = serial
                    .split("INPUT:PULSE:")
                    .nth(1)
                    .and_then(|s| s.lines().next())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                {
                    println!(
                        "{} pulse_us={} trigger_cycles={:?} cycles={} instructions={} reboots={}",
                        $chip,
                        result,
                        widths.borrow(),
                        m.bus.cycles(),
                        m.insns(),
                        m.reboots
                    );
                    assert!(result.abs_diff(5800) <= 50);
                    assert_eq!(m.reboots, 0);
                    return;
                }
            }
            panic!(
                "missing pulse result: {} widths={:?}",
                String::from_utf8_lossy(&m.console.uart0),
                widths.borrow()
            );
        }
    };
}
firmware!(
    external_s3_sonar,
    esp32s3::machine([0; 6]),
    "esp32s3",
    240_000_000,
    1,
    2,
    "esp32s3_rev0_rom.elf"
);
firmware!(
    external_c3_sonar,
    esp32c3::machine([0; 6], 8 * 1024 * 1024),
    "esp32c3",
    160_000_000,
    0,
    1,
    "esp32c3_rev3_rom.elf"
);
firmware!(
    external_c6_sonar,
    esp32c6::machine([0; 6], 8 * 1024 * 1024),
    "esp32c6",
    160_000_000,
    0,
    1,
    "esp32c6_rev0_rom.elf"
);

#[test]
fn released_pad_resolves_output_pulls_and_interrupts() {
    let mut gpio = esp_periph::Gpio::new();
    for pin in [0, 7, 32, 48] {
        let bit = 1u64 << pin;
        gpio.pin[pin] = (3 << 7) | (1 << 13);
        gpio.set_input(pin as u8, false);
        gpio.set_pulls(pin as u8, true, false);
        assert_eq!(gpio.input & bit, 0);
        assert!(gpio.release_input(pin as u8));
        assert_ne!(gpio.input & bit, 0);
        assert_ne!(gpio.status & bit, 0);
        let (out, ena, shift) = if pin < 32 {
            (4, 0x24, pin)
        } else {
            (0x10, 0x30, pin - 32)
        };
        gpio.write(ena, 1 << shift);
        assert_eq!(gpio.input & bit, 0);
        gpio.write(out, 1 << shift);
        assert_ne!(gpio.input & bit, 0);
        gpio.set_input(pin as u8, false);
        assert!(!gpio.level(pin as u8));
        gpio.release_input(pin as u8);
        assert!(gpio.level(pin as u8));
        gpio.set_pulls(pin as u8, false, true);
        gpio.write(ena + 4, 1 << shift);
        assert!(!gpio.level(pin as u8));
        gpio.set_pulls(pin as u8, false, false);
        assert!(gpio.level(pin as u8));
    }
    let before = gpio.input;
    assert!(!gpio.release_input(255));
    assert!(!gpio.set_input(255, false));
    gpio.set_pulls(255, false, true);
    assert_eq!(gpio.input, before);
}

struct Released;
impl BoardModel for Released {
    fn name(&self) -> &'static str {
        "released"
    }
    fn released_inputs(&self) -> Vec<u8> {
        vec![4]
    }
}
macro_rules! pulls {
    ($name:ident, $ctor:expr, $mux:expr, $gpio:expr) => {
        #[test]
        fn $name() {
            let mut m = $ctor;
            m.bus.board = Box::new(Released);
            m.bus.gpio_set_input(4, false);
            m.bus.write32($mux + 20, 1 << 8).unwrap();
            assert_eq!(m.bus.gpio_input() & 16, 0);
            m.bus.attach_board_devices();
            assert_ne!(m.bus.read32($gpio + 0x3c).unwrap() & 16, 0);
            m.bus.write32($mux + 20, 1 << 7).unwrap();
            assert_eq!(m.bus.read32($gpio + 0x3c).unwrap() & 16, 0);
            m.bus.gpio_set_input(4, true);
            m.bus.tick(256);
            assert_eq!(m.bus.read32($gpio + 0x3c).unwrap() & 16, 0);
            m.bus.reboot([0; 6]);
            m.bus.write32($mux + 20, 1 << 7).unwrap();
            assert_eq!(m.bus.read32($gpio + 0x3c).unwrap() & 16, 0);
        }
    };
}
pulls!(
    s3_pull_registers,
    esp32s3::machine([0; 6]),
    0x60009000,
    0x60004000
);
pulls!(
    c3_pull_registers,
    esp32c3::machine([0; 6], 8 * 1024 * 1024),
    0x60009000,
    0x60004000
);
pulls!(
    c6_pull_registers,
    esp32c6::machine([0; 6], 8 * 1024 * 1024),
    0x60090000,
    0x60091000
);

struct Keypad {
    pins: [u8; 8],
    key: Rc<RefCell<Option<(usize, usize)>>>,
    enabled: u64,
    output: u64,
    low: Option<u8>,
    edges: Vec<BoardEdge>,
}
impl BoardModel for Keypad {
    fn name(&self) -> &'static str {
        "keypad"
    }
    fn gpio_output_at(&mut self, cycle: u64, _: &[(u8, bool)], enabled: u64, output: u64) {
        self.enabled = enabled;
        self.output = output;
        self.advance_to(cycle);
    }
    fn advance_to(&mut self, cycle: u64) {
        let low = self.key.borrow().and_then(|(row, col)| {
            (self.enabled & !self.output & (1 << self.pins[4 + col]) != 0).then_some(self.pins[row])
        });
        if low != self.low {
            if let Some(pin) = low {
                self.edges.push(BoardEdge {
                    cycle,
                    pin,
                    level: false,
                });
            }
            self.low = low;
        }
    }
    fn released_inputs(&self) -> Vec<u8> {
        self.pins[..4]
            .iter()
            .copied()
            .filter(|&pin| Some(pin) != self.low)
            .collect()
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> {
        std::mem::take(&mut self.edges)
    }
}

macro_rules! keypad_firmware {
    ($name:ident, $ctor:expr, $chip:literal, $pins:expr, $rom:literal) => {
        #[test]
        #[ignore = "requires retained Arduino 3.3.8 input firmware; set GPIO_INPUT_FIRMWARE"]
        fn $name() {
            use std::{env, fs, path::PathBuf};
            let root = PathBuf::from(
                env::var_os("GPIO_INPUT_FIRMWARE")
                    .expect("set GPIO_INPUT_FIRMWARE to extracted input fixture binaries"),
            );
            let dir = root.join($chip);
            let rom = PathBuf::from(
                env::var_os("ESP32SIM_ROM_DIR")
                    .expect("set ESP32SIM_ROM_DIR to mask ROM directory"),
            );
            let mut m = $ctor;
            m.bus.set_flash_size(8 * 1024 * 1024);
            m.console.capture = true;
            let key = Rc::new(RefCell::new(None));
            m.bus.board = Box::new(Keypad {
                pins: $pins,
                key: key.clone(),
                enabled: 0,
                output: 0,
                low: None,
                edges: vec![],
            });
            m.bus.attach_board_devices();
            m.load_rom(&fs::read(rom.join($rom)).unwrap()).unwrap();
            for offset in fs::read_to_string(dir.join("flash.txt")).unwrap().lines() {
                m.write_flash(
                    offset.parse().unwrap(),
                    &fs::read(dir.join(offset)).unwrap(),
                )
                .unwrap();
            }
            m.boot_rom();
            let mut mode = false;
            let mut phase = 0;
            let mut next = 0.0;
            while m.seconds() < 3.0 {
                assert!(matches!(m.run(500_000), esp_soc::Stop::MaxInsns));
                let serial = String::from_utf8_lossy(&m.console.uart0).into_owned();
                if !mode && serial.contains("INPUT:READY") {
                    m.bus.uart_input(0, b"K");
                    mode = true;
                }
                if serial.contains("INPUT:KEYPAD") && m.seconds() >= next && phase < 6 {
                    *key.borrow_mut() = if phase % 2 == 0 {
                        Some([(0, 0), (1, 1), (3, 3)][phase / 2])
                    } else {
                        None
                    };
                    phase += 1;
                    next = m.seconds() + 0.1;
                }
                if phase == 6 && m.seconds() >= next {
                    let keys: Vec<_> = serial
                        .lines()
                        .filter(|s| s.starts_with("INPUT:KEY:"))
                        .collect();
                    println!(
                        "{} keys={:?} cycles={} instructions={} reboots={}",
                        $chip,
                        keys,
                        m.bus.cycles(),
                        m.insns(),
                        m.reboots
                    );
                    assert_eq!(keys, ["INPUT:KEY:1", "INPUT:KEY:5", "INPUT:KEY:D"]);
                    assert_eq!(m.reboots, 0);
                    return;
                }
            }
            panic!(
                "missing keypad result: {}",
                String::from_utf8_lossy(&m.console.uart0)
            );
        }
    };
}
keypad_firmware!(
    external_s3_keypad,
    esp32s3::machine([0; 6]),
    "esp32s3",
    [1, 2, 3, 4, 5, 6, 7, 8],
    "esp32s3_rev0_rom.elf"
);
keypad_firmware!(
    external_c3_keypad,
    esp32c3::machine([0; 6], 8 * 1024 * 1024),
    "esp32c3",
    [0, 1, 3, 4, 5, 6, 7, 10],
    "esp32c3_rev3_rom.elf"
);
keypad_firmware!(
    external_c6_keypad,
    esp32c6::machine([0; 6], 8 * 1024 * 1024),
    "esp32c6",
    [0, 1, 2, 3, 6, 7, 14, 16],
    "esp32c6_rev0_rom.elf"
);

#[test]
fn classic_release_retains_mux_pull_and_external_precedence() {
    let mut m = esp32::machine([0; 6], 4 * 1024 * 1024);
    let pin = 4;
    let mask = 1 << pin;
    let mux = 0x3ff49048;
    m.bus.write32(mux, (2 << 12) | (1 << 9) | (1 << 8)).unwrap();
    m.bus.gpio_set_input(pin, false);
    assert_eq!(m.bus.read32(0x3ff4403c).unwrap() & mask, 0);
    m.bus.gpio_release_input(pin);
    assert_eq!(m.bus.read32(0x3ff4403c).unwrap() & mask, mask);
    m.bus.gpio_set_input(pin, true);
    m.bus.write32(mux, (2 << 12) | (1 << 9) | (1 << 7)).unwrap();
    assert_eq!(m.bus.read32(0x3ff4403c).unwrap() & mask, mask);
    m.bus.gpio_release_input(pin);
    assert_eq!(m.bus.read32(0x3ff4403c).unwrap() & mask, 0);
}

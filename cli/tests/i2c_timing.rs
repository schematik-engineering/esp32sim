use emu_core::Bus;
use esp_periph::{
    i2c::{I2c, I2cDevice, Reg8Device, INT_END_DETECT, INT_NACK, INT_TRANS_COMPLETE},
    Device,
};
use esp_soc::{BoardModel, SocBus};
use std::sync::{Arc, Mutex};

fn configure(i2c: &mut I2c) {
    for (off, value) in [
        (0, 199),
        (0x38, 200),
        (0x40, 199),
        (0x44, 199),
        (0x48, 199),
        (0x4c, 199),
        (0x54, 0),
    ] {
        i2c.write(off, value);
    }
}
fn program(i2c: &mut I2c, bytes: &[u8], ops: &[(u32, u32)]) {
    i2c.write(0x24, u32::MAX);
    for &b in bytes {
        i2c.write(0x1c, b.into());
    }
    for (i, &(op, n)) in ops.iter().enumerate() {
        i2c.write(0x58 + 4 * i as u32, op << 11 | 1 << 8 | n);
    }
    i2c.write(4, 1 << 5);
}
#[test]
fn byte_deadlines_delay_fifo_callbacks_and_interrupts() {
    let mut i2c = I2c::new();
    configure(&mut i2c);
    i2c.attach(0x42, Box::new(Reg8Device::new("timed", &[(0, 0xa5)])));
    i2c.write(0x28, INT_TRANS_COMPLETE);
    program(&mut i2c, &[0x85], &[(6, 0), (1, 1), (3, 1), (2, 0)]);
    assert_eq!(
        i2c.read(8) & 16,
        16,
        "TRANS_START must leave the controller busy"
    );
    assert_eq!(i2c.int_raw, 0);
    assert!(!i2c.irq());
    assert_eq!(i2c.next_deadline(), Some(800));
    i2c.tick(799);
    assert_eq!(i2c.read(0x58) >> 31, 0);
    i2c.tick(1);
    assert_eq!(i2c.read(0x58) >> 31, 1);
    assert_eq!(i2c.next_deadline(), Some(7200));
    i2c.tick(7200);
    assert_eq!(i2c.read(8) >> 8 & 63, 0);
    i2c.tick(7199);
    assert_eq!(i2c.read(8) >> 8 & 63, 0);
    i2c.tick(1);
    assert_eq!(i2c.read(0x1c), 0xa5);
    i2c.tick(799);
    assert_eq!(i2c.int_raw, 0);
    i2c.tick(1);
    assert_eq!(i2c.int_raw, INT_TRANS_COMPLETE);
    assert!(i2c.irq());
    assert_eq!(i2c.read(8) & 16, 0);
    assert_eq!(i2c.next_deadline(), None);
}
#[test]
fn timed_nack_end_continuation_and_reset() {
    let mut i2c = I2c::new();
    configure(&mut i2c);
    program(&mut i2c, &[0x84], &[(6, 0), (1, 1), (2, 0)]);
    i2c.tick(7999);
    assert_eq!(i2c.int_raw, 0);
    i2c.tick(1);
    assert_eq!(i2c.int_raw, INT_NACK);
    i2c.attach(0x42, Box::new(Reg8Device::new("timed", &[(0, 42)])));
    program(&mut i2c, &[0x85], &[(6, 0), (1, 1), (4, 0)]);
    i2c.tick(8000);
    assert_eq!(i2c.int_raw, 0);
    i2c.tick(1);
    assert_eq!(i2c.int_raw, INT_END_DETECT);
    program(&mut i2c, &[], &[(3, 1), (2, 0)]);
    i2c.tick(8000);
    assert_eq!(i2c.read(0x1c), 42);
    assert_eq!(i2c.int_raw, INT_TRANS_COMPLETE);
    program(&mut i2c, &[0x85], &[(6, 0), (1, 1), (3, 1), (2, 0)]);
    i2c.tick(400);
    i2c.write(4, 1 << 10);
    i2c.tick(100000);
    assert_eq!(i2c.int_raw, 0);
    assert_eq!(i2c.next_deadline(), None);
    assert!(i2c.has_device(0x42));
}

#[derive(Default)]
struct SensorState {
    now: u64,
    busy: u64,
    ready: Option<u64>,
    reads: Vec<(u64, u64)>,
    early: usize,
    commands: Vec<(u16, u64)>,
}
struct Scd4x {
    state: Arc<Mutex<SensorState>>,
    hz: u64,
    command: Vec<u8>,
    response: std::collections::VecDeque<u8>,
    measurement: bool,
}
impl I2cDevice for Scd4x {
    fn pins(&self) -> Option<(u8, u8)> {
        Some((8, 9))
    }
    fn start(&mut self, read: bool) -> bool {
        let mut s = self.state.lock().unwrap();
        if read {
            let pair = (s.now, s.busy);
            s.reads.push(pair);
            if s.now < s.busy {
                s.early += 1;
                return false;
            }
            !self.response.is_empty()
        } else {
            self.command.clear();
            true
        }
    }
    fn write(&mut self, b: u8) -> bool {
        self.command.push(b);
        if self.command.len() != 2 {
            return true;
        }
        let cmd = u16::from_be_bytes([self.command[0], b]);
        let mut s = self.state.lock().unwrap();
        let now = s.now;
        s.commands.push((cmd, now));
        if now < s.busy {
            s.early += 1;
            return false;
        }
        self.response.clear();
        self.measurement = false;
        let words = match cmd {
            0x3f86 => {
                s.ready = None;
                s.busy = now + self.hz / 2;
                return true;
            }
            0x21b1 => {
                s.ready = Some(now + 5 * self.hz);
                return true;
            }
            0xe4b8 => vec![u16::from(s.ready.is_some_and(|at| now >= at))],
            0xec05 if s.ready.is_some_and(|at| now >= at) => {
                self.measurement = true;
                vec![800, 26214, 32768]
            }
            _ => return false,
        };
        s.busy = now + self.hz / 1000;
        for word in words {
            let bytes = word.to_be_bytes();
            let mut crc = 0xffu8;
            for byte in bytes {
                crc ^= byte;
                for _ in 0..8 {
                    crc = if crc & 0x80 != 0 {
                        (crc << 1) ^ 0x31
                    } else {
                        crc << 1
                    };
                }
            }
            self.response.extend([bytes[0], bytes[1], crc]);
        }
        true
    }
    fn read(&mut self) -> u8 {
        let byte = self.response.pop_front().unwrap_or(0xff);
        if self.measurement && self.response.is_empty() {
            let mut s = self.state.lock().unwrap();
            s.ready = Some(s.now + 5 * self.hz);
            self.measurement = false;
        }
        byte
    }
}
struct SensorBoard {
    state: Arc<Mutex<SensorState>>,
    hz: u64,
}
impl BoardModel for SensorBoard {
    fn name(&self) -> &'static str {
        "scd4x-timing-test"
    }
    fn advance_to(&mut self, now: u64) {
        self.state.lock().unwrap().now = now;
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn I2cDevice>)> {
        vec![(
            0,
            0x62,
            Box::new(Scd4x {
                state: self.state.clone(),
                hz: self.hz,
                command: Vec::new(),
                response: Default::default(),
                measurement: false,
            }),
        )]
    }
}
fn firmware<S: esp_soc::Soc>(mut m: esp_soc::Machine<S>, chip: &str) -> esp_soc::Machine<S> {
    use std::{env, fs, path::PathBuf, time::Instant};
    let root = PathBuf::from(
        env::var_os("I2C_TIMING_FIRMWARE_DIR")
            .expect("set I2C_TIMING_FIRMWARE_DIR to EX211 timing/.pio/build"),
    );
    let roms = PathBuf::from(
        env::var_os("ESP32SIM_ROM_DIR")
            .expect("set ESP32SIM_ROM_DIR to the absolute ROM directory"),
    );
    m.console.capture = true;
    m.load_rom(&fs::read(roms.join(S::ROM_ELF)).unwrap())
        .unwrap();
    for (off, name) in [
        (0, "bootloader.bin"),
        (0x8000, "partitions.bin"),
        (0x10000, "firmware.bin"),
    ] {
        m.write_flash(off, &fs::read(root.join(chip).join(name)).unwrap())
            .unwrap();
    }
    m.boot_rom();
    let start = Instant::now();
    while m.seconds() < 20.0 {
        assert!(matches!(m.run(1_000_000), esp_soc::Stop::MaxInsns));
        if String::from_utf8_lossy(&m.console.uart0).contains("TIMING DONE") {
            break;
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let serial = String::from_utf8_lossy(&m.console.uart0);
    println!(
        "{chip} output: {}",
        serial
            .lines()
            .filter(|s| s.starts_with("SCD ") || s.starts_with("HEAVY "))
            .collect::<Vec<_>>()
            .join("; ")
    );
    assert!(serial.contains("TIMING DONE"), "{serial}");
    assert_eq!(m.reboots, 0);
    println!(
        "TIMING chip={chip} cycles={} insns={} wall={wall:.9}",
        m.bus.cycles(),
        m.insns()
    );
    m
}
fn check_sensor<S: esp_soc::Soc>(
    m: esp_soc::Machine<S>,
    chip: &str,
    state: Arc<Mutex<SensorState>>,
) {
    let m = firmware(m, chip);
    {
        let s = state.lock().unwrap();
        println!(
            "{chip} commands={:?} reads={:?} early={}",
            s.commands, s.reads, s.early
        );
    }
    let serial = String::from_utf8_lossy(&m.console.uart0);
    for line in [
        "SCD stop 0",
        "SCD start 0",
        "SCD ready 0 1",
        "SCD sample 0 800 25.00 50.00",
    ] {
        assert!(serial.contains(line), "missing {line}: {serial}");
    }
    let s = state.lock().unwrap();
    assert_eq!(
        s.reads
            .iter()
            .filter(|(now, deadline)| now >= deadline)
            .count(),
        2
    );
    assert_eq!(
        s.early,
        s.reads
            .iter()
            .filter(|(now, deadline)| now < deadline)
            .count()
    );
    assert_eq!(s.early, 0);
    assert_eq!(
        s.commands.len(),
        4,
        "the final fixture must need no retries"
    );
}
#[test]
#[ignore = "requires EX211 Sensirion Arduino firmware and ROM ELFs"]
fn external_scd4x_s3() {
    let state = Arc::new(Mutex::new(SensorState::default()));
    let mut m = esp32s3::machine([0; 6]);
    m.bus.board = Box::new(SensorBoard {
        state: state.clone(),
        hz: 240_000_000,
    });
    m.bus.attach_board_devices();
    check_sensor(m, "s3", state);
}
#[test]
#[ignore = "requires EX211 Sensirion Arduino firmware and ROM ELFs"]
fn external_scd4x_c3() {
    let state = Arc::new(Mutex::new(SensorState::default()));
    let mut m = esp32c3::machine([0; 6], 4 << 20);
    m.bus.board = Box::new(SensorBoard {
        state: state.clone(),
        hz: 160_000_000,
    });
    m.bus.attach_board_devices();
    check_sensor(m, "c3", state);
}
#[test]
#[ignore = "requires EX211 Sensirion Arduino firmware and ROM ELFs"]
fn external_scd4x_c6() {
    let state = Arc::new(Mutex::new(SensorState::default()));
    let mut m = esp32c6::machine([0; 6], 8 << 20);
    m.bus.board = Box::new(SensorBoard {
        state: state.clone(),
        hz: 160_000_000,
    });
    m.bus.attach_board_devices();
    check_sensor(m, "c6", state);
}
#[test]
#[ignore = "requires EX211 I2C-heavy Arduino firmware and S3 ROM"]
fn external_i2c_heavy_s3() {
    let mut m = esp32s3::machine([0; 6]);
    m.bus.board = Box::new(esp_soc::NoBoard);
    m.bus.periph.i2c[0].attach(0x42, Box::new(Reg8Device::new("heavy", &[(0, 0xa5)])));
    let m = firmware(m, "heavy");
    assert!(
        String::from_utf8_lossy(&m.console.uart0).contains("HEAVY count=5000 sum=825000 errors=0")
    );
    assert_eq!(m.bus.periph.i2c[0].transactions, 5000);
}

#[test]
fn dividers_repeated_start_and_classic_clock_layout() {
    for (classic, clock, expected_byte) in [
        (false, 0, 7200),
        (false, 1 | (2 << 8) | (1 << 14), 18000),
        (false, 1 << 20, 16458),
        (true, 0, 7200),
    ] {
        let mut i2c = if classic {
            I2c::new_classic()
        } else {
            I2c::new()
        };
        configure(&mut i2c);
        if classic {
            for (off, value) in [
                (0, 399),
                (0x38, 393),
                (0x40, 400),
                (0x44, 400),
                (0x48, 400),
                (0x4c, 400),
            ] {
                i2c.write(off, value);
            }
        } else {
            i2c.write(0x54, clock);
        }
        i2c.attach(0x42, Box::new(Reg8Device::new("timing", &[(0, 42)])));
        let start = if classic { 0 } else { 6 };
        let stop = if classic { 3 } else { 2 };
        program(
            &mut i2c,
            &[0x84, 0x85],
            &[(start, 0), (1, 1), (start, 0), (1, 1), (stop, 0)],
        );
        let setup = i2c.next_deadline().unwrap();
        i2c.tick(setup);
        assert_eq!(i2c.next_deadline(), Some(expected_byte));
        i2c.tick(expected_byte);
        assert_eq!(i2c.next_deadline(), Some(setup));
        i2c.tick(setup);
        i2c.tick(expected_byte);
        assert_eq!(i2c.int_raw, 0);
        i2c.tick(setup);
        assert_eq!(i2c.int_raw, INT_TRANS_COMPLETE);
    }
}

struct ClockProbe(Arc<Mutex<(u64, Vec<u64>)>>);
impl I2cDevice for ClockProbe {
    fn start(&mut self, _: bool) -> bool {
        let mut s = self.0.lock().unwrap();
        let now = s.0;
        s.1.push(now);
        true
    }
    fn write(&mut self, _: u8) -> bool {
        true
    }
    fn read(&mut self) -> u8 {
        42
    }
}
struct ProbeBoard(Arc<Mutex<(u64, Vec<u64>)>>);
impl BoardModel for ProbeBoard {
    fn name(&self) -> &'static str {
        "i2c-clock-probe"
    }
    fn advance_to(&mut self, now: u64) {
        self.0.lock().unwrap().0 = now;
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn I2cDevice>)> {
        vec![(0, 0x42, Box::new(ClockProbe(self.0.clone())))]
    }
}
fn bus_timing(
    bus: &mut impl SocBus,
    base: u32,
    div: u32,
    classic: bool,
    probe: &Arc<Mutex<(u64, Vec<u64>)>>,
) {
    for (off, value) in [
        (0, 199),
        (0x38, 200),
        (0x40, 199),
        (0x44, 199),
        (0x48, 199),
        (0x4c, 199),
    ] {
        bus.write32(base + off, value).unwrap();
    }
    if classic {
        for (off, value) in [
            (0, 399),
            (0x38, 393),
            (0x40, 400),
            (0x44, 400),
            (0x48, 400),
            (0x4c, 400),
        ] {
            bus.write32(base + off, value).unwrap();
        }
    }
    bus.write32(base + 0x28, INT_TRANS_COMPLETE).unwrap();
    bus.write32(base + 0x1c, 0x84).unwrap();
    bus.write32(base + 0x58, if classic { 0 } else { 6 << 11 })
        .unwrap();
    bus.write32(base + 0x5c, 1 << 11 | 1 << 8 | 1).unwrap();
    bus.write32(base + 0x60, if classic { 3 << 11 } else { 2 << 11 })
        .unwrap();
    bus.write32(base + 4, 1 << 5).unwrap();
    assert_eq!(bus.read32(base + 0x2c).unwrap(), 0);
    bus.tick(8000 * div - 1);
    assert_eq!(bus.read32(base + 0x2c).unwrap(), 0);
    assert!(probe.lock().unwrap().1.is_empty());
    bus.tick(1);
    bus.read32(base + 8).unwrap();
    assert_eq!(probe.lock().unwrap().1, [u64::from(8000 * div)]);
    bus.tick(800 * div - 1);
    assert_eq!(bus.read32(base + 0x2c).unwrap(), 0);
    bus.tick(1);
    assert_eq!(bus.read32(base + 0x2c).unwrap(), INT_TRANS_COMPLETE);
}
#[test]
fn timed_callbacks_and_interrupt_status_on_every_chip() {
    let probe = Arc::new(Mutex::new((0, Vec::new())));
    let mut s3 = esp32s3::machine([0; 6]);
    s3.bus.board = Box::new(ProbeBoard(probe.clone()));
    s3.bus.attach_board_devices();
    bus_timing(&mut s3.bus, 0x60013000, 3, false, &probe);
    *probe.lock().unwrap() = (0, Vec::new());
    let mut c3 = esp32c3::machine([0; 6], 4 << 20);
    c3.bus.board = Box::new(ProbeBoard(probe.clone()));
    c3.bus.attach_board_devices();
    bus_timing(&mut c3.bus, 0x60013000, 2, false, &probe);
    *probe.lock().unwrap() = (0, Vec::new());
    let mut c6 = esp32c6::machine([0; 6], 8 << 20);
    c6.bus.board = Box::new(ProbeBoard(probe.clone()));
    c6.bus.attach_board_devices();
    // The C6 PCR divider doubles the elapsed time without changing the I2C registers.
    c6.bus.write32(0x60096024, 1 << 12).unwrap();
    bus_timing(&mut c6.bus, 0x60004000, 4, false, &probe);
    *probe.lock().unwrap() = (0, Vec::new());
    let mut classic = esp32::machine([0; 6], 4 << 20);
    classic.bus.board = Box::new(ProbeBoard(probe.clone()));
    classic.bus.attach_board_devices();
    for (pin, mux, signal) in [(21, 0x7c, 30), (22, 0x80, 29)] {
        classic
            .bus
            .write32(0x3ff49000 + mux, 2 << 12 | 1 << 9 | 1 << 8)
            .unwrap();
        classic
            .bus
            .write32(0x3ff44130 + 4 * signal, 1 << 7 | pin)
            .unwrap();
        classic.bus.write32(0x3ff44530 + 4 * pin, signal).unwrap();
    }
    bus_timing(&mut classic.bus, 0x3ff53000, 3, true, &probe);
}

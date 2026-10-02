use emu_core::Bus;
use esp_periph::i2c::{I2c, I2cDevice, Reg8Device, INT_NACK};
use esp_soc::{BoardModel, SocBus};
use std::sync::{Arc, Mutex};

#[derive(Default, Debug)]
struct Seen {
    writes: Vec<u8>,
    stops: usize,
    resets: usize,
}

// VL53L1X-style 16-bit registers: address at 0x0001, identity at 0x010f.
struct Sensor {
    addr: u8,
    pins: Option<(u8, u8)>,
    general: bool,
    reject: bool,
    nack_data: bool,
    call: bool,
    phase: u8,
    reg: u16,
    seen: Arc<Mutex<Seen>>,
}
impl I2cDevice for Sensor {
    fn address(&self, _: u8) -> u8 {
        self.addr
    }
    fn matches_address(&self, _: u8, addr: u8, read: bool) -> bool {
        addr == self.addr || (self.general && addr == 0 && !read)
    }
    fn start_address(&mut self, addr: u8, read: bool) -> bool {
        self.call = addr == 0;
        if !read {
            self.phase = 0;
        }
        !self.reject
    }
    fn pins(&self) -> Option<(u8, u8)> {
        self.pins
    }
    fn write(&mut self, b: u8) -> bool {
        let mut seen = self.seen.lock().unwrap();
        seen.writes.push(b);
        if self.call {
            if b == 6 {
                seen.resets += 1;
            }
            return b == 6 && !self.nack_data;
        }
        match self.phase {
            0 => {
                self.reg = u16::from(b) << 8;
                self.phase = 1;
            }
            1 => {
                self.reg |= u16::from(b);
                self.phase = 2;
            }
            _ => {
                if self.reg == 1 {
                    self.addr = b & 0x7f;
                }
                self.reg += 1;
            }
        }
        true
    }
    fn read(&mut self) -> u8 {
        if self.reg == 0x010f {
            0xea
        } else {
            0xff
        }
    }
    fn stop(&mut self) {
        self.seen.lock().unwrap().stops += 1;
    }
}
fn sensor(addr: u8, pins: Option<(u8, u8)>, general: bool) -> (Sensor, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    (
        Sensor {
            addr,
            pins,
            general,
            reject: false,
            nack_data: false,
            call: false,
            phase: 0,
            reg: 0,
            seen: seen.clone(),
        },
        seen,
    )
}
fn commands(write: &mut impl FnMut(u32, u32), bytes: &[u8], ops: &[(u32, u32)], classic: bool) {
    write(0x24, u32::MAX);
    write(0x18, 3 << 12);
    for &b in bytes {
        write(0x1c, u32::from(b));
    }
    for (i, &(op, n)) in ops.iter().enumerate() {
        let op = match (op, classic) {
            (6, true) => 0,
            (3, true) => 2,
            (2, true) => 3,
            _ => op,
        };
        write(0x58 + i as u32 * 4, (op << 11) | (1 << 8) | n);
    }
    write(4, 1 << 5);
}
fn send(i2c: &mut I2c, bytes: &[u8], stop: bool) -> bool {
    commands(
        &mut |o, v| i2c.write(o, v),
        bytes,
        &[
            (6, 0),
            (1, bytes.len() as u32),
            (if stop { 2 } else { 4 }, 0),
        ],
        false,
    );
    i2c.int_raw & INT_NACK == 0
}

#[test]
fn shared_address_change_replacement_and_pin_matching() {
    let mut i2c = I2c::new();
    let (a, seen) = sensor(0x29, Some((8, 9)), false);
    let (other, untouched) = sensor(0x29, Some((6, 7)), false);
    i2c.attach(0x29, Box::new(a));
    i2c.attach(0x29, Box::new(other));
    i2c.set_pins(Some((8, 9)));
    assert!(send(&mut i2c, &[0x52, 0, 1, 0x30, 0xaa], true));
    assert_eq!(seen.lock().unwrap().writes, [0, 1, 0x30, 0xaa]);
    assert!(untouched.lock().unwrap().writes.is_empty());
    assert!(!send(&mut i2c, &[0x52], true));
    assert!(send(&mut i2c, &[0x60], true));
    assert!(i2c.has_device(0x30));
    // Replacement at the current address and same pins must leave only the new device.
    let (replacement, replacement_seen) = sensor(0x30, Some((8, 9)), false);
    i2c.attach(0x30, Box::new(replacement));
    assert!(send(&mut i2c, &[0x60, 1, 15], true));
    assert_eq!(replacement_seen.lock().unwrap().writes, [1, 15]);
    assert_eq!(seen.lock().unwrap().writes, [0, 1, 0x30, 0xaa]);
    assert!(i2c.detach(0x30).is_some());
    assert!(!send(&mut i2c, &[0x60], true));
    i2c.set_pins(Some((6, 7)));
    assert!(send(&mut i2c, &[0x52], true));
}

#[test]
fn shared_general_call_fanout_ack_lifetime_and_filtering() {
    let mut i2c = I2c::new();
    let (mut a, first) = sensor(0x58, Some((8, 9)), true);
    a.nack_data = true;
    let (b, second) = sensor(0x59, None, true);
    let (wrong, wrong_seen) = sensor(0x58, Some((6, 7)), true);
    let (mut rejecting, rejected) = sensor(0x5a, None, true);
    rejecting.reject = true;
    i2c.attach(0x58, Box::new(a));
    i2c.attach(0x59, Box::new(b));
    i2c.attach(0x58, Box::new(wrong));
    i2c.attach(0x5a, Box::new(rejecting));
    i2c.attach(0x60, Box::new(Reg8Device::new("ordinary", &[])));
    i2c.set_pins(Some((8, 9)));
    assert!(send(&mut i2c, &[0, 6], true));
    for seen in [&first, &second] {
        let seen = seen.lock().unwrap();
        assert_eq!((seen.resets, seen.stops), (1, 1));
    }
    assert!(wrong_seen.lock().unwrap().writes.is_empty());
    assert!(rejected.lock().unwrap().writes.is_empty());
    assert!(!send(&mut i2c, &[1], true)); // General-call reads are not accepted.
    assert!(!send(&mut i2c, &[0, 7], true)); // No data ACK.
    assert!(send(&mut i2c, &[0], false)); // END retains both recipients.
    i2c.detach(0x58);
    commands(&mut |o, v| i2c.write(o, v), &[6], &[(1, 1), (2, 0)], false);
    assert_eq!(second.lock().unwrap().resets, 2);
    assert_eq!(first.lock().unwrap().resets, 1);
    assert!(send(&mut i2c, &[0], false));
    i2c.set_pins(Some((6, 7)));
    commands(&mut |o, v| i2c.write(o, v), &[6], &[(1, 1), (2, 0)], false);
    assert_ne!(i2c.int_raw & INT_NACK, 0);
    i2c.clear_devices();
    assert!(!send(&mut i2c, &[0, 6], true));
}

struct Board;
impl BoardModel for Board {
    fn name(&self) -> &'static str {
        "i2c-address-test"
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn I2cDevice>)> {
        vec![
            (0, 0x29, Box::new(sensor(0x29, None, false).0)),
            (0, 0x58, Box::new(sensor(0x58, None, true).0)),
        ]
    }
}
fn chip(bus: &mut impl SocBus, base: u32, classic: bool) {
    for (bytes, ack) in [
        (&[0x52, 0, 1, 0x30][..], true),
        (&[0x52][..], false),
        (&[0x60, 1, 15][..], true),
        (&[0, 6][..], true),
        (&[1][..], false),
    ] {
        commands(
            &mut |o, v| bus.write32(base + o, v).unwrap(),
            bytes,
            &[(6, 0), (1, bytes.len() as u32), (2, 0)],
            classic,
        );
        assert_eq!(bus.read32(base + 0x20).unwrap() & INT_NACK == 0, ack);
    }
    commands(
        &mut |o, v| bus.write32(base + o, v).unwrap(),
        &[0x61],
        &[(6, 0), (1, 1), (3, 1), (2, 0)],
        classic,
    );
    assert_eq!(bus.read32(base + 0x1c).unwrap(), 0xea);
}
#[test]
fn s3_address_and_general_call() {
    let mut m = esp32s3::machine([0; 6]);
    m.bus.board = Box::new(Board);
    m.bus.attach_board_devices();
    chip(&mut m.bus, 0x60013000, false);
}
#[test]
fn c3_address_and_general_call() {
    let mut m = esp32c3::machine([0; 6], 4 << 20);
    m.bus.board = Box::new(Board);
    m.bus.attach_board_devices();
    chip(&mut m.bus, 0x60013000, false);
}
#[test]
fn c6_address_and_general_call() {
    let mut m = esp32c6::machine([0; 6], 4 << 20);
    m.bus.board = Box::new(Board);
    m.bus.attach_board_devices();
    chip(&mut m.bus, 0x60004000, false);
}
#[test]
fn classic_address_and_general_call() {
    let mut m = esp32::machine([0; 6], 4 << 20);
    m.bus.board = Box::new(Board);
    m.bus.attach_board_devices();
    for (pin, mux, signal) in [(21, 0x7c, 30), (22, 0x80, 29)] {
        m.bus
            .write32(0x3ff49000 + mux, (2 << 12) | (1 << 9) | (1 << 8))
            .unwrap();
        m.bus
            .write32(0x3ff44130 + 4 * signal, (1 << 7) | pin)
            .unwrap();
        m.bus.write32(0x3ff44530 + 4 * pin, signal).unwrap();
    }
    chip(&mut m.bus, 0x3ff53000, true);
}

fn firmware<S: esp_soc::Soc>(
    mut m: esp_soc::Machine<S>,
    chip: &str,
    i2c: &mut impl FnMut(&mut S::Bus) -> &mut I2c,
) {
    let (ranging, _) = sensor(0x29, Some((8, 9)), false);
    let (gas_a, a) = sensor(0x58, Some((8, 9)), true);
    let (gas_b, b) = sensor(0x59, Some((8, 9)), true);
    for (addr, dev) in [(0x29, ranging), (0x58, gas_a), (0x59, gas_b)] {
        i2c(&mut m.bus).attach(addr, Box::new(dev));
    }
    let m = run_firmware(m, chip, "I2C_FIRMWARE_DIR", "I2C DONE");
    let serial = String::from_utf8_lossy(&m.console.uart0);
    let lines: Vec<_> = serial.lines().filter(|s| s.starts_with("I2C ")).collect();
    println!("{chip}: {lines:?}");
    assert_eq!(
        lines,
        [
            "I2C before 234",
            "I2C change 0",
            "I2C old 2",
            "I2C after 234",
            "I2C reset 0",
            "I2C DONE"
        ]
    );
    assert_eq!(a.lock().unwrap().resets, 1);
    assert_eq!(b.lock().unwrap().resets, 1);
    assert_eq!(m.reboots, 0);
    println!(
        "{chip}: cycles={} insns={} reboots={}",
        m.bus.cycles(),
        m.insns(),
        m.reboots
    );
}
#[test]
#[ignore = "requires EX211 Arduino 3.3.8 firmware and ROM ELFs"]
fn external_arduino_s3_i2c_address() {
    firmware(esp32s3::machine([0; 6]), "s3", &mut |b| {
        &mut b.periph.i2c[0]
    });
}
#[test]
#[ignore = "requires EX211 Arduino 3.3.8 firmware and ROM ELFs"]
fn external_arduino_c3_i2c_address() {
    firmware(esp32c3::machine([0; 6], 4 << 20), "c3", &mut |b| {
        &mut b.periph.i2c
    });
}
#[test]
#[ignore = "requires EX211 Arduino 3.3.8 firmware and ROM ELFs"]
fn external_arduino_c6_i2c_address() {
    firmware(esp32c6::machine([0; 6], 8 << 20), "c6", &mut |b| {
        &mut b.periph.i2c
    });
}

// Register-level PCA9685 fixture; PWM timing and output pins are outside this bus test.
struct Pca9685 {
    regs: [u8; 256],
    starts: Vec<(u8, bool)>,
    reject: bool,
    resets: usize,
}
impl Default for Pca9685 {
    fn default() -> Self {
        let mut regs = [0; 256];
        regs[0] = 0x11;
        regs[1] = 4;
        regs[2..6].copy_from_slice(&[0xe2, 0xe4, 0xe8, 0xe0]);
        regs[0xfe] = 30;
        Self {
            regs,
            starts: Vec::new(),
            reject: false,
            resets: 0,
        }
    }
}
struct PcaDevice {
    state: Arc<Mutex<Pca9685>>,
    pins: (u8, u8),
    pointer: u8,
    first: bool,
    reset: bool,
}
impl I2cDevice for PcaDevice {
    fn pins(&self) -> Option<(u8, u8)> {
        Some(self.pins)
    }
    fn matches_address(&self, configured: u8, addr: u8, read: bool) -> bool {
        let s = self.state.lock().unwrap();
        addr == configured
            || (addr == 0 && !read)
            || (0..4).any(|n| {
                s.regs[0] & (1 << n) != 0 && addr == s.regs[if n == 0 { 5 } else { 5 - n }] >> 1
            })
    }
    fn start_address(&mut self, addr: u8, read: bool) -> bool {
        let mut s = self.state.lock().unwrap();
        s.starts.push((addr, read));
        self.first = !read;
        self.reset = addr == 0;
        !s.reject
    }
    fn write(&mut self, byte: u8) -> bool {
        let mut s = self.state.lock().unwrap();
        if self.reset {
            if byte != 6 {
                return false;
            }
            s.regs = Pca9685::default().regs;
            s.resets += 1;
        } else if self.first {
            self.pointer = byte;
            self.first = false;
        } else {
            s.regs[self.pointer as usize] = byte;
            if s.regs[0] & 0x20 != 0 {
                self.pointer = self.pointer.wrapping_add(1);
            }
        }
        true
    }
    fn read(&mut self) -> u8 {
        let s = self.state.lock().unwrap();
        let byte = s.regs[self.pointer as usize];
        if s.regs[0] & 0x20 != 0 {
            self.pointer = self.pointer.wrapping_add(1);
        }
        byte
    }
}
fn pca(state: &Arc<Mutex<Pca9685>>, pins: (u8, u8)) -> Box<dyn I2cDevice> {
    Box::new(PcaDevice {
        state: state.clone(),
        pins,
        pointer: 0,
        first: false,
        reset: false,
    })
}

#[test]
fn shared_programmable_aliases_and_all_call() {
    let mut i2c = I2c::new();
    let a = Arc::new(Mutex::new(Pca9685::default()));
    let b = Arc::new(Mutex::new(Pca9685::default()));
    let wrong = Arc::new(Mutex::new(Pca9685::default()));
    i2c.attach(0x40, pca(&a, (8, 9)));
    i2c.attach(0x41, pca(&b, (8, 9)));
    i2c.attach(0x40, pca(&wrong, (6, 7)));
    i2c.set_pins(Some((8, 9)));
    assert!(send(&mut i2c, &[0xe0, 8, 0x55], true));
    assert_eq!(a.lock().unwrap().regs[8], 0x55);
    assert_eq!(
        b.lock().unwrap().regs[8],
        0x55,
        "all-call must reach the second device"
    );
    assert!(wrong.lock().unwrap().starts.is_empty());
    // Each programmable subaddress is consulted afresh after register writes.
    for (reg, bit, addr) in [(2, 8, 0x72), (3, 4, 0x73), (4, 2, 0x74), (5, 1, 0x75)] {
        assert!(send(&mut i2c, &[0x80, 0, 0], true));
        assert!(send(&mut i2c, &[0x80, reg, addr << 1], true));
        assert!(!send(&mut i2c, &[addr << 1], true));
        assert!(send(&mut i2c, &[0x80, 0, bit], true));
        assert!(send(&mut i2c, &[addr << 1, 8, addr], true));
        assert_eq!(a.lock().unwrap().regs[8], addr);
        assert_eq!(a.lock().unwrap().starts.last(), Some(&(addr, false)));
        assert_eq!(b.lock().unwrap().regs[8], 0x55);
    }
    assert!(send(&mut i2c, &[0, 6], true));
    assert_eq!(a.lock().unwrap().resets, 1);
    assert_eq!(b.lock().unwrap().resets, 1);
    assert!(send(&mut i2c, &[0x80, 8, 0xf0], true));
    assert!(send(&mut i2c, &[0x82, 8, 0x5a], true));
    // Repeated-start reads notify both recipients and resolve their open-drain data.
    commands(
        &mut |o, v| i2c.write(o, v),
        &[0xe0, 8, 0xe1],
        &[(6, 0), (1, 2), (6, 0), (1, 1), (3, 1), (2, 0)],
        false,
    );
    assert_eq!(i2c.read(0x1c), 0x50);
    for state in [&a, &b] {
        assert_eq!(state.lock().unwrap().starts.last(), Some(&(0x70, true)));
    }
    // A matching device can refuse this transaction without hiding a later ACK.
    a.lock().unwrap().reject = true;
    assert!(send(&mut i2c, &[0xe0, 8, 0x33], true));
    assert_eq!(a.lock().unwrap().regs[8], 0xf0);
    assert_eq!(b.lock().unwrap().regs[8], 0x33);
    a.lock().unwrap().reject = false;
    assert!(send(&mut i2c, &[0x80, 0, 0], true));
    assert!(send(&mut i2c, &[0x82, 0, 0], true));
    assert!(!send(&mut i2c, &[0xe0], true));
    assert!(send(&mut i2c, &[0x80], true));
    assert!(!send(&mut i2c, &[1], true));
    assert!(i2c.has_device(0x40));
    assert!(!i2c.has_device(0x70)); // Aliases do not create extra attachments.
}

fn run_firmware<S: esp_soc::Soc>(
    mut m: esp_soc::Machine<S>,
    chip: &str,
    input: &str,
    marker: &str,
) -> esp_soc::Machine<S> {
    use std::{env, fs, path::PathBuf};
    let root = PathBuf::from(
        env::var_os(input)
            .unwrap_or_else(|| panic!("set {input} to the EX211 PlatformIO .pio/build directory")),
    );
    let roms = PathBuf::from(
        env::var_os("ESP32SIM_ROM_DIR")
            .expect("set ESP32SIM_ROM_DIR to the absolute ROM directory"),
    );
    m.console.capture = true;
    m.load_rom(&fs::read(roms.join(S::ROM_ELF)).unwrap())
        .unwrap();
    for (offset, name) in [
        (0, "bootloader.bin"),
        (0x8000, "partitions.bin"),
        (0x10000, "firmware.bin"),
    ] {
        m.write_flash(offset, &fs::read(root.join(chip).join(name)).unwrap())
            .unwrap();
    }
    m.boot_rom();
    for _ in 0..500 {
        assert!(matches!(m.run(1_000_000), esp_soc::Stop::MaxInsns));
        if String::from_utf8_lossy(&m.console.uart0).contains(marker) {
            break;
        }
    }
    m
}

struct PcaBoard([Arc<Mutex<Pca9685>>; 3]);
impl BoardModel for PcaBoard {
    fn name(&self) -> &'static str {
        "pca9685-alias-test"
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn I2cDevice>)> {
        vec![
            (0, 0x40, pca(&self.0[0], (8, 9))),
            (0, 0x41, pca(&self.0[1], (8, 9))),
            (0, 0x40, pca(&self.0[2], (6, 7))),
        ]
    }
}
fn pca_firmware<S: esp_soc::Soc>(
    m: esp_soc::Machine<S>,
    chip: &str,
    states: [Arc<Mutex<Pca9685>>; 3],
) {
    let m = run_firmware(m, chip, "PCA_FIRMWARE_DIR", "PCA DONE");
    let serial = String::from_utf8_lossy(&m.console.uart0);
    let lines: Vec<_> = serial.lines().filter(|s| s.starts_with("PCA ")).collect();
    println!("{chip}: {lines:?}");
    assert_eq!(
        lines,
        [
            "PCA begin 1 1 1",
            "PCA main 300 450 and 256",
            "PCA group 600 600",
            "PCA disabled 600 700",
            "PCA reset 0",
            "PCA DONE"
        ]
    );
    for state in &states[..2] {
        let s = state.lock().unwrap();
        assert_eq!(s.resets, 1);
        assert_eq!(s.regs[0], 0x11);
        assert!(s.starts.contains(&(0x70, false)));
        assert!(s.starts.contains(&(0x70, true)));
        assert!(s.starts.contains(&(0, false)));
    }
    assert!(states[2].lock().unwrap().starts.is_empty());
    assert_eq!(m.reboots, 0);
    println!(
        "{chip}: cycles={} insns={} reboots={}",
        m.bus.cycles(),
        m.insns(),
        m.reboots
    );
}
#[test]
#[ignore = "requires EX211 Adafruit PCA9685 Arduino firmware and ROM ELFs"]
fn external_arduino_s3_pca9685() {
    let states = std::array::from_fn(|_| Arc::new(Mutex::new(Pca9685::default())));
    let mut m = esp32s3::machine([0; 6]);
    m.bus.board = Box::new(PcaBoard(states.clone()));
    m.bus.attach_board_devices();
    pca_firmware(m, "s3", states);
}
#[test]
#[ignore = "requires EX211 Adafruit PCA9685 Arduino firmware and ROM ELFs"]
fn external_arduino_c3_pca9685() {
    let states = std::array::from_fn(|_| Arc::new(Mutex::new(Pca9685::default())));
    let mut m = esp32c3::machine([0; 6], 4 << 20);
    m.bus.board = Box::new(PcaBoard(states.clone()));
    m.bus.attach_board_devices();
    pca_firmware(m, "c3", states);
}
#[test]
#[ignore = "requires EX211 Adafruit PCA9685 Arduino firmware and ROM ELFs"]
fn external_arduino_c6_pca9685() {
    let states = std::array::from_fn(|_| Arc::new(Mutex::new(Pca9685::default())));
    let mut m = esp32c6::machine([0; 6], 8 << 20);
    m.bus.board = Box::new(PcaBoard(states.clone()));
    m.bus.attach_board_devices();
    pca_firmware(m, "c6", states);
}

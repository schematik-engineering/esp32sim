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
    use std::{env, fs, path::PathBuf};
    let root = PathBuf::from(
        env::var_os("I2C_FIRMWARE_DIR")
            .expect("set I2C_FIRMWARE_DIR to EX211 PlatformIO .pio/build"),
    );
    let roms = PathBuf::from(
        env::var_os("ESP32SIM_ROM_DIR")
            .expect("set ESP32SIM_ROM_DIR to the absolute ROM directory"),
    );
    let (ranging, _) = sensor(0x29, Some((8, 9)), false);
    let (gas_a, a) = sensor(0x58, Some((8, 9)), true);
    let (gas_b, b) = sensor(0x59, Some((8, 9)), true);
    for (addr, dev) in [(0x29, ranging), (0x58, gas_a), (0x59, gas_b)] {
        i2c(&mut m.bus).attach(addr, Box::new(dev));
    }
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
        if String::from_utf8_lossy(&m.console.uart0).contains("I2C DONE") {
            break;
        }
    }
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

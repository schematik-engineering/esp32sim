use super::*;
use std::collections::BTreeMap;
const FW: &[u8; 86016] = include_bytes!("vl53l5cx/vl53l5cx_firmware.bin");
const CONFIG: &[u8; 972] = include_bytes!("vl53l5cx/vl53l5cx_default_configuration.bin");
const XTALK: &[u8; 776] = include_bytes!("vl53l5cx/vl53l5cx_default_xtalk.bin");
const NVM_CMD: &[u8; 40] = include_bytes!("vl53l5cx/vl53l5cx_get_nvm_cmd.bin");
fn swap(b: &mut [u8]) {
    for word in b.chunks_exact_mut(4) {
        word.reverse();
    }
}
fn nvm() -> Vec<u8> {
    let mut b = Vec::with_capacity(492);
    for header in NVM_CMD[..32].chunks_exact(4) {
        let h = u32::from_be_bytes(header.try_into().unwrap());
        let typ = (h & 15) as usize;
        let size = ((h >> 4) & 4095) as usize;
        b.extend(header);
        b.resize(
            b.len() + size * if (1..13).contains(&typ) { typ } else { 1 },
            0,
        );
    }
    b.extend([0, 0, 0, 15]);
    b
}
fn offset(res: u8) -> Vec<u8> {
    // Synthetic ideal factory profile: zero per-zone offset and signal correction.
    let mut b = nvm()[..488].to_vec();
    if res == 16 {
        b[0x10..0x18].copy_from_slice(&[15, 4, 4, 0, 8, 16, 16, 7]);
    }
    b.copy_within(8..488, 0);
    b[480..488].copy_from_slice(&[0, 0, 0, 15, 3, 1, 1, 0xe4]);
    b
}
fn xtalk(res: u8) -> Vec<u8> {
    let mut b = XTALK.to_vec();
    if res == 16 {
        b[8..16].copy_from_slice(&[15, 4, 4, 23, 8, 16, 16, 7]);
        b[32..40].copy_from_slice(&[0, 120, 0, 8, 0, 0, 0, 8]);
        swap(&mut b);
        let grid: Vec<u32> = b[52..308]
            .chunks_exact(4)
            .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
            .collect();
        b[52..308].fill(0);
        for y in 0..4 {
            for x in 0..4 {
                let a = 2 * x + 16 * y;
                let v = (grid[a] + grid[a + 1] + grid[a + 8] + grid[a + 9]) / 4;
                b[52 + 4 * (x + 4 * y)..56 + 4 * (x + 4 * y)].copy_from_slice(&v.to_le_bytes());
            }
        }
        swap(&mut b);
        b[308..312].copy_from_slice(&[160, 252, 1, 0]);
        b[120..124].fill(0);
    }
    b
}
pub(super) struct Vl53l5cx {
    s: SampleState,
    page: u8,
    start: u16,
    fw_cursor: usize,
    fw_bad: bool,
    boot: u64,
    mcu_running: bool,
    mailbox: Vec<u8>,
    mail_start: Option<u16>,
    mail_next: u16,
    response: Vec<u8>,
    status: [u8; 4],
    command_ready: u64,
    offset_ok: bool,
    xtalk_ok: bool,
    configured: bool,
    dci: BTreeMap<u16, Vec<u8>>,
    next: Option<u64>,
    frame: Vec<u8>,
    snapshot: Vec<u8>,
    stream: u8,
    stopped: bool,
}
impl Vl53l5cx {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[58] = 500.;
        Self {
            s,
            page: 0,
            start: 0,
            fw_cursor: 0,
            fw_bad: false,
            boot: 0,
            mcu_running: false,
            mailbox: vec![0; 1024],
            mail_start: None,
            mail_next: 0,
            response: vec![],
            status: [0; 4],
            command_ready: 0,
            offset_ok: false,
            xtalk_ok: false,
            configured: false,
            dci: BTreeMap::new(),
            next: None,
            frame: vec![255, 0, 0, 0],
            snapshot: vec![],
            stream: 0,
            stopped: false,
        }
    }
    fn reset(&mut self) {
        self.stopped = false;
        self.stream = 0;
        self.status = [0; 4];
        self.response.clear();
        self.snapshot.clear();
        self.command_ready = 0;
        self.fw_cursor = 0;
        self.fw_bad = false;
        self.mcu_running = false;
        self.boot = self.s.now + self.s.ticks(1000);
        self.offset_ok = false;
        self.xtalk_ok = false;
        self.configured = false;
        self.dci.clear();
        self.next = None;
        self.frame = vec![255, 0, 0, 0];
        self.mail_start = None;
        self.s.readings[58] = f64::NAN;
        self.s.publish(1);
    }
    fn resolution(&self) -> u8 {
        self.dci
            .get(&0x5450)
            .filter(|v| v.len() == 8)
            .map_or(16, |v| v[0].saturating_mul(v[1]))
    }
    fn frequency(&self) -> u8 {
        self.dci.get(&0x5458).map_or(1, |v| v[1])
    }
    fn fail(&mut self) -> bool {
        self.status = [0, 0, 0x80, 0];
        false
    }
    fn output(&mut self) -> bool {
        let (Some(list), Some(enables), Some(config)) = (
            self.dci.get(&0xcd78),
            self.dci.get(&0xcd68),
            self.dci.get(&0xcd60),
        ) else {
            return false;
        };
        let length = u32::from_le_bytes(config[..4].try_into().unwrap()) as usize;
        if !(24..=4096).contains(&length) {
            return false;
        }
        let mut b = vec![0; 16];
        let enabled = u32::from_le_bytes(enables[..4].try_into().unwrap());
        let zones = self.resolution() as usize;
        let expected = [
            0x0000000du32,
            0x54b400c0,
            0x54c00040,
            0x54d00104,
            0x55d00404,
            0xcf7c0401,
            0xcfbc0404,
            0xd2bc0402,
            0xd33c0402,
            0xd43c0401,
            0xd47c0401,
            0xcc5008c0,
        ];
        if enables != &[255, 15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192]
            || config[4..8] != 13u32.to_le_bytes()
        {
            return false;
        }
        for (i, chunk) in list.chunks_exact(4).enumerate() {
            if enabled & (1 << i) == 0 {
                continue;
            }
            let h = u32::from_le_bytes(chunk.try_into().unwrap());
            let mut supported = expected[i];
            if (1..13).contains(&(supported & 15)) {
                supported = (supported & 0xffff000f) | ((zones as u32) << 4);
            }
            if h != supported {
                return false;
            }
            let typ = (h & 15) as usize;
            let size = ((h >> 4) & 4095) as usize;
            let idx = (h >> 16) as u16;
            let n = if (1..13).contains(&typ) {
                size * typ
            } else {
                size
            };
            if n > 1024 {
                return false;
            }
            b.extend(chunk);
            let start = b.len();
            b.resize(start + n, 0);
            match idx {
                0xcf7c if n == zones => b[start..].fill(1),
                0xd33c if n == zones * 2 => {
                    let raw = (self.s.inputs[58] * 4.).round() as i16;
                    for z in 0..zones {
                        b[start + z * 2..start + z * 2 + 2].copy_from_slice(&raw.to_le_bytes());
                    }
                }
                0xd47c if n == zones => b[start..].fill(5),
                _ => {}
            }
        }
        b.extend([0; 4]);
        if b.len() != length {
            return false;
        }
        swap(&mut b);
        b[..4].copy_from_slice(&[self.stream, 5, 5, 0x10]);
        self.frame = b;
        true
    }
    fn command(&mut self, start: u16) -> bool {
        if !self.mcu_running || self.fw_bad || self.fw_cursor != FW.len() {
            return self.fail();
        }
        let data = self.mailbox[(start - 0x2c00) as usize..].to_vec();
        let ok = match start {
            0x2fd8 if data == NVM_CMD => {
                self.response = nvm();
                self.status = [2, 0, 0, 0];
                true
            }
            0x2e18 if data == offset(self.resolution()) => {
                self.offset_ok = true;
                true
            }
            0x2cf8 if data == xtalk(self.resolution()) => {
                self.xtalk_ok = true;
                true
            }
            0x2c34 if data == CONFIG && self.offset_ok && self.xtalk_ok => {
                let mut p = 0;
                while p + 4 < CONFIG.len() {
                    let idx = u16::from_be_bytes([CONFIG[p], CONFIG[p + 1]]);
                    if idx == 0 {
                        break;
                    }
                    let len = ((CONFIG[p + 2] as usize) << 4) | (CONFIG[p + 3] as usize >> 4);
                    if p + 4 + len > CONFIG.len() {
                        return self.fail();
                    }
                    let mut v = CONFIG[p + 4..p + 4 + len].to_vec();
                    swap(&mut v);
                    self.dci.insert(idx, v);
                    p += 4 + len;
                }
                self.configured = true;
                true
            }
            0x2ffc
                if data == [0, 3, 0, 0] && self.configured && self.offset_ok && self.xtalk_ok =>
            {
                let hz = self.frequency();
                let max = if self.resolution() == 64 { 15 } else { 60 };
                if hz == 0 || hz > max || !self.output() {
                    false
                } else {
                    self.stopped = false;
                    self.frame.fill(0);
                    self.frame[0] = 255;
                    self.s.readings[58] = f64::NAN;
                    self.s.publish(1);
                    self.next = Some(self.s.now + self.s.ticks(1_000_000 / hz as u64));
                    true
                }
            }
            _ if self.configured && data.len() >= 12 => {
                let idx = u16::from_be_bytes([data[0], data[1]]);
                let n = ((data[2] as usize) << 4) | (data[3] as usize >> 4);
                if start == 0x2ff4 && data[4..] == [0, 0, 0, 15, 0, 2, 0, 8] {
                    if let Some(v) = self.dci.get(&idx).filter(|v| v.len() == n) {
                        self.response = data[..4].to_vec();
                        let mut wire = v.clone();
                        swap(&mut wire);
                        self.response.extend(wire);
                        self.response.extend(&data[4..]);
                        true
                    } else {
                        false
                    }
                } else if data.len() == n + 12
                    && data[n + 4..n + 10] == [0, 0, 0, 15, 5, 1]
                    && u16::from_be_bytes([data[n + 10], data[n + 11]]) as usize == n + 8
                {
                    let allowed = match idx {
                        0x5450 => n == 8,
                        0x5458 | 0xcf78 | 0xcd5c => n == 4,
                        0xad38 | 0xcd68 => n == 16,
                        0xcd60 => n == 8,
                        0xcd78 => n == 48,
                        _ => false,
                    };
                    if !allowed {
                        false
                    } else {
                        let mut v = data[4..n + 4].to_vec();
                        swap(&mut v);
                        if (idx == 0xcf78 && v != [1, 0, 1, 0])
                            || (idx == 0xcd5c && v != [1, 0, 0, 0])
                        {
                            false
                        } else if idx == 0x5450
                            && !((v[0] == 4 && v[1] == 4) || (v[0] == 8 && v[1] == 8))
                        {
                            false
                        } else if idx == 0x5458
                            && (v[1] == 0 || v[1] > if self.resolution() == 64 { 15 } else { 60 })
                        {
                            false
                        } else {
                            if idx == 0x5450 {
                                self.offset_ok = false;
                                self.xtalk_ok = false;
                            }
                            self.dci.insert(idx, v);
                            true
                        }
                    }
                } else {
                    false
                }
            }
            _ => false,
        };
        if ok {
            if !(start == 0x2fd8 && data == NVM_CMD) {
                self.status = [0, 3, 0, 0];
            }
            self.command_ready = self.s.now + self.s.ticks(1000);
            true
        } else {
            self.fail()
        }
    }
}
impl RegisterSensor for Vl53l5cx {
    fn format(&self) -> WireFormat {
        WireFormat::Address16
    }
    fn select_extended(&mut self, reg: u16) {
        self.start = reg;
        if self.page == 2 && reg == 0 {
            self.snapshot = self.frame.clone();
        }
    }
    fn read_extended(&self, reg: u16) -> u8 {
        if reg == 0x7fff {
            return self.page;
        }
        match self.page {
            0 => match reg {
                0 => 0xf0,
                1 => 2,
                6 => {
                    if self.s.now < self.boot {
                        if self.mcu_running {
                            1
                        } else {
                            0
                        }
                    } else if self.stopped {
                        0x80
                    } else if self.mcu_running {
                        0
                    } else {
                        1
                    }
                }
                _ => 0,
            },
            1 if reg == 0x21 => 0x10,
            2 => {
                if (0x2c00..0x2c04).contains(&reg) {
                    if self.s.now < self.command_ready {
                        0
                    } else {
                        self.status[(reg - 0x2c00) as usize]
                    }
                } else if reg >= 0x2c04 {
                    self.response
                        .get((reg - 0x2c04) as usize)
                        .copied()
                        .unwrap_or(0)
                } else {
                    self.snapshot.get(reg as usize).copied().unwrap_or(0)
                }
            }
            _ => 0,
        }
    }
    fn write_extended(&mut self, reg: u16, value: u8) -> bool {
        self.sync();
        if reg == 0x7fff && self.start == 0x7fff {
            if ![0, 1, 2, 9, 10, 11].contains(&value) {
                return false;
            }
            if (9..=11).contains(&value) {
                let expected = (value as usize - 9) * 0x8000;
                if self.fw_cursor != expected {
                    self.fw_bad = true;
                    return false;
                }
            }
            self.page = value;
            return true;
        }
        match self.page {
            9..=11 => {
                let offset = (self.page as usize - 9) * 0x8000 + reg as usize;
                if offset != self.fw_cursor || FW.get(offset) != Some(&value) {
                    self.fw_bad = true;
                    return false;
                }
                self.fw_cursor += 1;
                true
            }
            0 => match reg {
                0xa if value == 3 => {
                    self.reset();
                    true
                }
                0xb if value == 1 => {
                    if self.fw_cursor != FW.len() || self.fw_bad {
                        return false;
                    }
                    self.mcu_running = true;
                    self.boot = self.s.now + self.s.ticks(1000);
                    true
                }
                0x14 if value == 1 => {
                    self.next = None;
                    self.stopped = true;
                    self.frame = vec![255, 0, 0, 0];
                    self.s.readings[58] = f64::NAN;
                    self.s.publish(1);
                    true
                }
                9 | 0xf | 0xa | 0xc | 0xe | 0x101 | 0x102 | 0x10a | 0x4002 | 0x103 | 0x400f
                | 0x21a | 0x219 | 0x21b | 0x114 | 0x115 | 0x116 | 0x117 | 0xb | 0x14 | 0x15 => true,
                _ => false,
            },
            1 => reg == 0x20,
            2 if reg == 3 => value == 0xd,
            2 if (0x2c04..=0x2fff).contains(&reg) => {
                if self.mail_start.is_none() || reg != self.mail_next {
                    self.mail_start = Some(reg);
                    self.mailbox.fill(0);
                }
                self.mailbox[(reg - 0x2c00) as usize] = value;
                self.mail_next = reg + 1;
                if reg == 0x2fff {
                    let start = self.mail_start.take().unwrap();
                    self.command(start)
                } else {
                    true
                }
            }
            _ => false,
        }
    }
    fn sync(&mut self) {
        self.s.time();
        if let Some(next) = self.next {
            if self.s.now >= next {
                let period = self
                    .s
                    .ticks(1_000_000 / self.frequency().max(1) as u64)
                    .max(1);
                let count = (self.s.now - next) / period + 1;
                self.next = Some(next + count * period);
                self.stream = ((self.stream as u64 + count) % 255) as u8;
                if self.output() {
                    self.s.readings[58] = (self.s.inputs[58] * 4.).round() / 4.;
                    self.s.publish(count);
                } else {
                    self.next = None;
                    self.s.readings[58] = f64::NAN;
                }
            }
        }
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _: u8, _: u16) -> bool {
        false
    }
    fn set(&mut self, f: u32, v: f64) -> bool {
        if f != 58 || !v.is_finite() || !(20. ..=4000.).contains(&v) {
            return false;
        }
        self.sync();
        self.s.inputs[58] = v;
        true
    }
    fn generation(&mut self) -> u32 {
        self.sync();
        self.s.generation
    }
    fn value(&mut self, f: u32) -> f64 {
        self.sync();
        self.s.value(f)
    }
}

#[cfg(test)]
mod vl53l5cx_tests {
    use super::*;
    fn device() -> (Arc<AtomicU64>, Vl53l5cx) {
        let c = Arc::new(AtomicU64::new(0));
        (c.clone(), Vl53l5cx::new(c, 1_000_000))
    }
    fn page(d: &mut Vl53l5cx, p: u8) -> bool {
        d.select_extended(0x7fff);
        d.write_extended(0x7fff, p)
    }
    fn send(d: &mut Vl53l5cx, addr: u16, b: &[u8]) -> bool {
        d.select_extended(addr);
        for (i, v) in b.iter().enumerate() {
            if !d.write_extended(addr + i as u16, *v) {
                return false;
            }
        }
        true
    }
    fn upload(d: &mut Vl53l5cx) {
        for (p, bytes) in [
            (9, &FW[..0x8000]),
            (10, &FW[0x8000..0x10000]),
            (11, &FW[0x10000..]),
        ] {
            assert!(page(d, p));
            for (i, chunk) in bytes.chunks(30).enumerate() {
                assert!(send(d, (i * 30) as u16, chunk));
            }
        }
        assert!(page(d, 0));
        assert!(send(d, 0xb, &[1]));
        assert!(page(d, 2));
    }
    fn init(d: &mut Vl53l5cx) {
        upload(d);
        assert!(send(d, 0x2fd8, NVM_CMD));
        assert_eq!(d.response, nvm());
        assert!(send(d, 0x2e18, &offset(16)));
        assert!(send(d, 0x2cf8, &xtalk(16)));
        assert!(send(d, 0x2c34, CONFIG));
    }
    fn dci(d: &mut Vl53l5cx, idx: u16, v: &[u8]) -> bool {
        let n = v.len();
        let mut b = vec![(idx >> 8) as u8, idx as u8, (n >> 4) as u8, (n << 4) as u8];
        let mut wire = v.to_vec();
        swap(&mut wire);
        b.extend(wire);
        b.extend([0, 0, 0, 15, 5, 1, ((n + 8) >> 8) as u8, (n + 8) as u8]);
        send(d, 0x3000 - b.len() as u16, &b)
    }
    fn configure(d: &mut Vl53l5cx, z: u8) {
        assert!(dci(
            d,
            0x5450,
            &[
                if z == 16 { 4 } else { 8 },
                if z == 16 { 4 } else { 8 },
                4,
                0,
                8,
                8,
                0,
                0
            ]
        ));
        assert!(send(d, 0x2e18, &offset(z)));
        assert!(send(d, 0x2cf8, &xtalk(z)));
        assert!(dci(d, 0x5458, &[0, 10, 0, 0]));
        let mut list = vec![];
        let mut length = 20;
        for mut h in [
            0x0000000du32,
            0x54b400c0,
            0x54c00040,
            0x54d00104,
            0x55d00404,
            0xcf7c0401,
            0xcfbc0404,
            0xd2bc0402,
            0xd33c0402,
            0xd43c0401,
            0xd47c0401,
            0xcc5008c0,
        ] {
            let typ = h & 15;
            if (1..13).contains(&typ) {
                h = (h & 0xffff000f) | ((z as u32) << 4);
            }
            let size = (h >> 4) & 4095;
            length += 4 + if (1..13).contains(&typ) {
                size * typ
            } else {
                size
            };
            list.extend(h.to_le_bytes());
        }
        assert!(dci(d, 0xcd78, &list));
        let mut config = length.to_le_bytes().to_vec();
        config.extend(13u32.to_le_bytes());
        assert!(dci(d, 0xcd60, &config));
        let mut enables = 0xfffu32.to_le_bytes().to_vec();
        enables.extend([0; 8]);
        enables.extend(0xc0000000u32.to_le_bytes());
        assert!(dci(d, 0xcd68, &enables));
    }
    #[test]
    fn vl53l5cx_identity_wiring_and_bounds() {
        let (_, mut d) = device();
        assert_eq!(d.read_extended(0), 0xf0);
        assert_eq!(d.read_extended(1), 2);
        for v in [19., 4001., f64::NAN, f64::INFINITY] {
            assert!(!d.set(58, v));
        }
        assert!(d.set(58, 20.));
        assert!(d.set(58, 4000.));
        assert!(!d.set(0, 20.));
        let c = SensorConfig {
            id: 0,
            sda: 4,
            scl: 5,
            address: 0x29,
            model: 58,
            shunt_milliohms: 0,
        };
        assert!(c.valid());
        assert!(!SensorConfig { address: 0x28, ..c }.valid());
        assert!(!SensorConfig { scl: 4, ..c }.valid());
    }
    #[test]
    fn vl53l5cx_exact_upload_and_configuration() {
        let (_, mut d) = device();
        init(&mut d);
        assert_eq!(d.fw_cursor, 86016);
        assert!(!d.fw_bad);
        assert!(d.configured);
        assert_eq!(d.resolution(), 16);
        assert_eq!(d.frequency(), 1);
        assert_eq!(d.dci[&0x5450], [4, 4, 4, 0, 8, 8, 0, 0]);
    }
    #[test]
    fn vl53l5cx_corrupted_truncated_and_out_of_order_uploads_fail() {
        let (_, mut d) = device();
        assert!(page(&mut d, 9));
        assert!(!send(&mut d, 0, &[FW[0] ^ 1]));
        assert!(d.fw_bad);
        let (_, mut d) = device();
        assert!(page(&mut d, 9));
        assert!(send(&mut d, 0, &FW[..20]));
        assert!(!page(&mut d, 10));
        let (_, mut d) = device();
        assert!(!page(&mut d, 11));
        assert!(page(&mut d, 2));
        assert!(!send(&mut d, 0x2c34, CONFIG));
    }
    #[test]
    fn vl53l5cx_configuration_and_xtalk_corruption_fail() {
        let (_, mut d) = device();
        upload(&mut d);
        let mut wrong = xtalk(16);
        wrong[100] ^= 1;
        assert!(!send(&mut d, 0x2cf8, &wrong));
        assert!(send(&mut d, 0x2e18, &offset(16)));
        assert!(send(&mut d, 0x2cf8, &xtalk(16)));
        let mut config = CONFIG.to_vec();
        config[10] ^= 1;
        assert!(!send(&mut d, 0x2c34, &config));
        assert!(!d.configured);
    }
    #[test]
    fn vl53l5cx_frames_all_zones_status_and_timing() {
        for z in [16, 64] {
            let (c, mut d) = device();
            init(&mut d);
            configure(&mut d, z);
            assert!(send(&mut d, 0x2ffc, &[0, 3, 0, 0]));
            assert_eq!(d.frame[0], 255);
            c.store(99_999, Ordering::Relaxed);
            d.sync();
            assert!(d.value(58).is_nan());
            c.store(100_000, Ordering::Relaxed);
            d.sync();
            assert_eq!(&d.frame[..4], &[1, 5, 5, 16]);
            let mut b = d.frame.clone();
            swap(&mut b);
            let mut p = 16;
            let mut seen = 0;
            while p + 4 < b.len() {
                let h = u32::from_le_bytes(b[p..p + 4].try_into().unwrap());
                let typ = h & 15;
                let n = (((h >> 4) & 4095) * if (1..13).contains(&typ) { typ } else { 1 }) as usize;
                match h >> 16 {
                    0xd33c => {
                        assert_eq!(n, z as usize * 2);
                        for v in b[p + 4..p + 4 + n].chunks_exact(2) {
                            assert_eq!(i16::from_le_bytes(v.try_into().unwrap()) / 4, 500);
                        }
                        seen += 1;
                    }
                    0xcf7c => {
                        assert!(b[p + 4..p + 4 + n].iter().all(|v| *v == 1));
                        seen += 1;
                    }
                    0xd47c => {
                        assert!(b[p + 4..p + 4 + n].iter().all(|v| *v == 5));
                        seen += 1;
                    }
                    _ => {}
                }
                p += 4 + n;
            }
            assert_eq!(seen, 3);
        }
    }
    #[test]
    fn vl53l5cx_partial_frame_snapshot_and_stop_restart() {
        let (c, mut d) = device();
        init(&mut d);
        configure(&mut d, 64);
        send(&mut d, 0x2ffc, &[0, 3, 0, 0]);
        c.store(100_000, Ordering::Relaxed);
        d.sync();
        d.select_extended(0);
        let saved = d.snapshot.clone();
        d.set(58, 1234.);
        c.store(200_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.snapshot, saved);
        assert_ne!(d.frame, saved);
        page(&mut d, 0);
        send(&mut d, 0x14, &[1]);
        assert_eq!(d.read_extended(6), 0x80);
        assert!(d.value(58).is_nan());
        page(&mut d, 2);
        assert!(send(&mut d, 0x2ffc, &[0, 3, 0, 0]));
        c.store(300_000, Ordering::Relaxed);
        d.sync();
        assert_eq!(d.value(58), 1234.);
    }
    #[test]
    fn vl53l5cx_instances_and_reset_reupload() {
        let (c, mut a) = device();
        let (_, mut b) = device();
        init(&mut a);
        configure(&mut a, 16);
        send(&mut a, 0x2ffc, &[0, 3, 0, 0]);
        c.store(100_000, Ordering::Relaxed);
        a.sync();
        assert_eq!(a.value(58), 500.);
        assert!(b.value(58).is_nan());
        page(&mut a, 0);
        send(&mut a, 0xa, &[3]);
        assert!(!a.configured);
        assert!(a.value(58).is_nan());
        init(&mut a);
        assert!(a.configured);
    }
    #[test]
    fn vl53l5cx_dci_invalid_commands_and_rate() {
        let (_, mut d) = device();
        init(&mut d);
        assert!(!dci(&mut d, 0x9999, &[0; 4]));
        assert!(!dci(&mut d, 0x5450, &[7, 7, 0, 0, 0, 0, 0, 0]));
        configure(&mut d, 64);
        assert!(!dci(&mut d, 0x5458, &[0, 16, 0, 0]));
        assert!(!dci(&mut d, 0x5458, &[0, 0, 0, 0]));
    }
    #[test]
    fn vl53l5cx_unchanged_uld_calibration_transform_golden() {
        let profile = nvm();
        assert_eq!(profile.len(), 492);
        assert_eq!(&profile[..4], &[0x54, 0, 0, 0x40]);
        assert_eq!(&profile[56..60], &[0x9e, 0x38, 4, 4]);
        assert_eq!(&profile[316..320], &[0x9f, 0x38, 4, 2]);
        assert!(profile[60..316].iter().all(|v| *v == 0));
        assert!(profile[320..448].iter().all(|v| *v == 0));
        assert_eq!(&profile[488..], &[0, 0, 0, 15]);
        assert_eq!(offset(16), include_bytes!("vl53l5cx/offset-16.bin"));
        assert_eq!(offset(64), include_bytes!("vl53l5cx/offset-64.bin"));
        assert_eq!(xtalk(16), include_bytes!("vl53l5cx/xtalk-16.bin"));
        assert_eq!(xtalk(64), include_bytes!("vl53l5cx/xtalk-64.bin"));
    }
}

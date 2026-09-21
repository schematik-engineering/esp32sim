use super::*;

// MLX90393 ABA-011, Melexis datasheet rev12 §§12,14–16 and temperature-compensation note rev4.
pub(super) struct Mlx90393 {
    s: SampleState,
    memory: [u16; 64],
    nvram: [u16; 64],
    request: Vec<u8>,
    mode: u8,
    axes: u8,
    next: Option<u64>,
    reset_until: u64,
    accessible_at: u64,
    store_until: Option<u64>,
    raw: [u16; 4],
    ready: bool,
    reference: Option<[i32; 4]>,
}
impl Mlx90393 {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.inputs[0] = 25.;
        let mut nvram = [0; 64];
        nvram[0] = 0x7c;
        nvram[0x24] = 46696; // nominal TREF at 35°C, 45.2 counts/°C
        Self {
            s,
            memory: nvram,
            nvram,
            request: Vec::new(),
            mode: 0,
            axes: 0,
            next: None,
            reset_until: 0,
            accessible_at: 0,
            store_until: None,
            raw: [0; 4],
            ready: false,
            reference: None,
        }
    }
    fn tcmp(&self) -> bool {
        self.memory[1] & 0x400 != 0
    }
    fn resolution(&self, axis: usize) -> u16 {
        (self.memory[2] >> (5 + axis * 2)) & 3
    }
    fn scale(&self, axis: usize) -> f64 {
        let hall0 = self.memory[0] & 15 == 0;
        let base = if axis == 2 {
            if hall0 {
                0.316
            } else {
                0.242
            }
        } else if hall0 {
            0.196
        } else {
            0.150
        };
        base * [5., 4., 3., 2.5, 2., 5. / 3., 4. / 3., 1.][((self.memory[0] >> 4) & 7) as usize]
            * f64::from(1u16 << self.resolution(axis))
    }
    fn conversion_us(&self) -> u64 {
        let osr = self.memory[2] & 3;
        let filter = (self.memory[2] >> 2) & 7;
        let osr2 = (self.memory[2] >> 11) & 3;
        u64::from((self.axes & 14).count_ones()) * (67 + 64 * (1 << osr) * (2 + (1 << filter)))
            + if self.axes & 1 != 0 || self.tcmp() {
                67 + 192 * (1 << osr2)
            } else {
                0
            }
            + 100
    }
    fn period(&self) -> u64 {
        let interval = u64::from(self.memory[1] & 63) * 20_000;
        self.s
            .ticks(
                self.conversion_us()
                    + interval
                    + if interval == 0 {
                        0
                    } else if self.mode == 0x40 {
                        580
                    } else {
                        360
                    },
            )
            .max(1)
    }
    fn config_valid(&self) -> bool {
        let hall = self.memory[0] & 15;
        let osr = self.memory[2] & 3;
        let filter = (self.memory[2] >> 2) & 7;
        matches!(hall, 0 | 12)
            && !(hall == 12 && osr + filter < 2)
            && !(self.tcmp() && (0..3).any(|axis| self.resolution(axis) > 1))
    }
    fn sample(&mut self, count: u64) {
        let t = (46244. + (self.s.inputs[0] - 25.) * 45.2).round() as u16;
        if self.axes & 1 != 0 || self.tcmp() {
            self.raw[0] = t;
            self.s.readings[0] = 25. + (f64::from(t) - 46244.) / 45.2;
        }
        for axis in 0..3 {
            if self.axes & (2 << axis) == 0 {
                continue;
            }
            let resolution = self.resolution(axis);
            let scale = self.scale(axis);
            let limit = match resolution {
                2 => 22000.,
                3 => 11000.,
                _ => 32768.,
            };
            let upper = if resolution < 2 { limit - 1. } else { limit };
            let mut code = (self.s.inputs[29 + axis] / scale)
                .round()
                .clamp(-limit, upper);
            let offset = if self.tcmp() {
                let centered = code + 32768. - f64::from(self.memory[4 + axis]);
                let tc = if t > self.memory[0x24] {
                    self.memory[3] >> 8
                } else {
                    self.memory[3] & 255
                };
                let correction =
                    (f64::from(tc) * (f64::from(t) - f64::from(self.memory[0x24])) / 128.).floor();
                code = (centered + (centered * correction / 4096.).floor()).clamp(-32768., 32767.);
                32768.
            } else {
                match resolution {
                    2 => 32768.,
                    3 => 16384.,
                    _ => 0.,
                }
            };
            self.raw[axis + 1] = (code + offset) as i32 as u16;
            self.s.readings[29 + axis] = code * scale;
        }
        if self.mode == 0x40 {
            let mut current = [i32::from(t), 0, 0, 0];
            for axis in 0..3 {
                current[axis + 1] = if self.tcmp() || self.resolution(axis) == 2 {
                    i32::from(self.raw[axis + 1]) - 32768
                } else if self.resolution(axis) == 3 {
                    i32::from(self.raw[axis + 1]) - 16384
                } else {
                    self.raw[axis + 1] as i16 as i32
                };
            }
            if let Some(reference) = self.reference {
                for axis in 0..4 {
                    let threshold = self.memory[match axis {
                        0 => 9,
                        3 => 8,
                        _ => 7,
                    }];
                    if self.axes & (1 << axis) != 0
                        && (current[axis] - reference[axis]).unsigned_abs() > u32::from(threshold)
                    {
                        self.ready = true;
                    }
                }
            }
            if self.reference.is_none() || self.memory[1] & 0x1000 != 0 {
                self.reference = Some(current);
            }
        } else {
            self.ready = true;
        }
        self.s.publish(count);
    }
    fn command(&mut self) {
        self.sync();
        let command = self.request[0];
        self.s.regs = [0xff; 256];
        let mut status = self.mode;
        if (self.s.now < self.reset_until || self.store_until.is_some()) && command != 0xf0 {
            self.s.regs[0] = status | 0x10;
            return;
        }
        if self.s.now < self.accessible_at && !(0x40..=0x4f).contains(&command) {
            self.s.regs[0] = status | 0x10;
            return;
        }
        match command {
            0x00 => {}
            0xf0 => {
                self.accessible_at = 0;
                self.store_until = None;
                self.memory = self.nvram;
                self.mode = 0;
                self.axes = 0;
                self.next = None;
                self.ready = false;
                self.reference = None;
                self.raw = [0; 4];
                self.s.readings = [f64::NAN; FIELD_COUNT];
                self.reset_until = self.s.now + self.s.ticks(1500);
                status = 4;
            }
            0x80 => {
                self.mode = 0;
                self.next = None;
                self.ready = false;
                status = 0;
            }
            0x10..=0x3f => {
                let mode = match command >> 4 {
                    1 => 0x80,
                    2 => 0x40,
                    _ => 0x20,
                };
                let axes = if command & 15 == 0 {
                    ((self.memory[1] >> 6) & 15) as u8
                } else {
                    command & 15
                };
                if self.mode != 0 || axes == 0 || !self.config_valid() {
                    status |= 0x10;
                } else {
                    self.mode = mode;
                    self.axes = axes;
                    self.ready = false;
                    self.reference = None;
                    self.next = Some(self.s.now + self.s.ticks(580 + self.conversion_us()).max(1));
                    status = mode;
                }
            }
            0x40..=0x4f => {
                let converted = self.axes | u8::from(self.tcmp());
                let axes = command & converted & 15;
                if !self.ready || axes == 0 {
                    status |= 0x10;
                }
                status |= axes.count_ones().saturating_sub(1) as u8;
                let mut at = 1;
                for axis in 0..4 {
                    if axes & (1 << axis) != 0 {
                        self.s.regs[at..at + 2].copy_from_slice(&self.raw[axis].to_be_bytes());
                        at += 2;
                    }
                }
                if axes != 0 {
                    self.ready = false;
                }
            }
            0x50 if self.mode == 0 && self.request[1] & 3 == 0 => {
                let word = self.memory[(self.request[1] >> 2) as usize];
                self.s.regs[1..3].copy_from_slice(&word.to_be_bytes());
            }
            0x60 if self.mode == 0 && self.request[3] & 3 == 0 && self.request[3] >> 2 < 32 => {
                let reg = (self.request[3] >> 2) as usize;
                self.memory[reg] = u16::from_be_bytes([self.request[1], self.request[2]]);
            }
            0xd0 if self.mode == 0 => self.memory = self.nvram,
            0xe0 if self.mode == 0 => self.store_until = Some(self.s.now + self.s.ticks(15_000)),
            _ => status |= 0x10,
        }
        self.s.regs[0] = status;
    }
}
impl RegisterSensor for Mlx90393 {
    fn format(&self) -> WireFormat {
        WireFormat::Command
    }
    fn address_ready(&self) -> bool {
        (self.memory[1] >> 13) & 3 != 2
    }
    fn start(&mut self, read: bool) {
        if !read {
            self.request.clear();
        } else if self.request.len()
            != match self.request.first() {
                Some(0x50) => 2,
                Some(0x60) => 4,
                Some(_) => 1,
                None => 0,
            }
        {
            self.s.regs[0] = self.mode | 0x10;
        }
    }
    fn sync(&mut self) {
        self.s.time();
        if self.store_until.is_some_and(|at| self.s.now >= at) {
            self.nvram = self.memory;
            self.store_until = None;
        }
        if self.mode == 0x20 && self.next.is_none() && self.s.now >= self.accessible_at {
            self.mode = 0;
        }
        let Some(at) = self.next else {
            return;
        };
        if self.s.now < at {
            return;
        }
        let count = if self.mode == 0x20 {
            1
        } else {
            1 + (self.s.now - at) / self.period()
        };
        let was_ready = self.ready;
        self.sample(count);
        if !was_ready && self.ready {
            self.accessible_at = at + (count - 1) * self.period() + self.s.ticks(132);
        }
        self.next = if self.mode == 0x20 {
            if self.s.now >= self.accessible_at {
                self.mode = 0;
            }
            None
        } else {
            Some(at + count * self.period())
        };
    }
    fn registers(&self) -> [u8; 256] {
        self.s.regs
    }
    fn write(&mut self, _reg: u8, value: u16) -> bool {
        if self.request.len() == 4 {
            self.s.regs[0] = self.mode | 0x10;
            return true;
        }
        self.request.push(value as u8);
        let expected = match self.request[0] {
            0x50 => 2,
            0x60 => 4,
            _ => 1,
        };
        if self.request.len() == expected {
            self.command();
        } else if self.request.len() > expected {
            self.s.regs[0] = self.mode | 0x10;
        }
        true
    }
    fn set(&mut self, field: u32, value: f64) -> bool {
        self.sync();
        let valid = value.is_finite()
            && match field {
                0 => (-40.0..=85.).contains(&value),
                29..=31 => (-50000.0..=50000.).contains(&value),
                _ => false,
            };
        if valid {
            self.s.inputs[field as usize] = value;
        }
        valid
    }
    fn generation(&mut self) -> u32 {
        self.sync();
        self.s.generation
    }
    fn value(&mut self, field: u32) -> f64 {
        self.sync();
        self.s.value(field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device() -> Mlx90393 {
        Mlx90393::new(Arc::new(AtomicU64::new(0)), 1_000_000)
    }
    fn command(d: &mut Mlx90393, bytes: &[u8]) -> [u8; 256] {
        d.start(false);
        for byte in bytes {
            assert!(d.write(0, u16::from(*byte)));
        }
        d.start(true);
        d.registers()
    }
    fn write(d: &mut Mlx90393, reg: u8, word: u16) {
        let [hi, lo] = word.to_be_bytes();
        assert_eq!(command(d, &[0x60, hi, lo, reg << 2])[0] & 0x10, 0);
    }
    fn time(d: &mut Mlx90393, at: u64) {
        d.s.clock.store(at, Ordering::Relaxed);
        d.sync();
    }
    fn measure(d: &mut Mlx90393) -> [u8; 256] {
        assert_eq!(command(d, &[0x3f])[0], 0x20);
        time(d, d.next.unwrap() + 132);
        command(d, &[0x4f])
    }
    #[test]
    fn mlx90393_command_transactions_ready_boundaries_reset_and_nonvolatile_memory() {
        let mut d = device();
        assert!(d.value(29).is_nan());
        assert_eq!(d.generation(), 0);
        assert_eq!(&command(&mut d, &[0x50, 0])[..3], &[0, 0, 0x7c]);
        assert_eq!(
            command(&mut d, &[0x60, 0x12])[0] & 0x10,
            0x10,
            "partial word is not committed"
        );
        assert_eq!(&command(&mut d, &[0x50, 0])[1..3], &[0, 0x7c]);
        assert_eq!(
            command(&mut d, &[0x60, 1, 2, 0x90])[0] & 0x10,
            0x10,
            "factory trim is read-only"
        );
        assert_eq!(command(&mut d, &[0x71])[0] & 0x10, 0x10);
        write(&mut d, 2, 14);
        write(&mut d, 10, 0x1234);
        assert_eq!(command(&mut d, &[0xe0])[0], 0);
        assert_eq!(d.nvram[10], 0);
        time(&mut d, 14999);
        assert_eq!(command(&mut d, &[0x60, 0x55, 0x55, 40])[0] & 16, 16);
        assert_eq!(d.memory[10], 0x1234);
        assert_eq!(d.nvram[10], 0);
        time(&mut d, 15000);
        assert_eq!(command(&mut d, &[0])[0], 0);
        assert_eq!(d.nvram[10], 0x1234);
        write(&mut d, 10, 0x9999);
        command(&mut d, &[0xd0]);
        assert_eq!(&command(&mut d, &[0x50, 40])[1..3], &[0x12, 0x34]);
        assert!(d.set(29, 150.));
        assert!(d.set(30, -300.));
        assert!(d.set(31, 60.5));
        assert!(d.set(0, -10.));
        assert_eq!(command(&mut d, &[0x3f])[0], 0x20);
        assert_eq!(d.next, Some(23820));
        assert_eq!(command(&mut d, &[0x4f])[0] & 0x10, 0x10);
        time(&mut d, 23819);
        assert_eq!(d.generation(), 0);
        time(&mut d, 23820);
        assert_eq!(d.generation(), 1);
        assert_eq!(
            &command(&mut d, &[0x4f])[..9],
            &[0x23, 0xae, 0x76, 3, 0xe8, 0xf8, 0x30, 0, 0xfa]
        );
        assert_eq!(
            command(&mut d, &[0x4f])[0] & 0x10,
            0x10,
            "same result cannot be read twice"
        );
        assert_eq!(d.value(29), 150.);
        assert_eq!(d.value(30), -300.);
        assert_eq!(d.value(31), 60.5);
        time(&mut d, 23951);
        assert_eq!(command(&mut d, &[0x50, 0])[0], 0x30);
        time(&mut d, 23952);
        assert_eq!(command(&mut d, &[0x50, 0])[0], 0);
        assert_eq!(command(&mut d, &[0xf0])[0], 4);
        assert!(d.value(29).is_nan());
        assert_eq!(d.s.inputs[29], 150.);
        time(&mut d, 25451);
        assert_eq!(command(&mut d, &[0])[0] & 0x10, 0x10);
        time(&mut d, 25452);
        assert_eq!(command(&mut d, &[0])[0], 0);
        assert_eq!(&command(&mut d, &[0x50, 40])[1..3], &[0x12, 0x34]);
        assert_eq!(d.memory[2], 14);
        for (field, value) in [
            (29, f64::NAN),
            (30, f64::INFINITY),
            (31, 50000.1),
            (0, -40.1),
            (1, 1.),
        ] {
            assert!(!d.set(field, value));
        }
    }
    #[test]
    fn mlx90393_gain_resolution_signed_formats_temperature_compensation_and_saturation() {
        for hall in [0u16, 12] {
            for gain in 0..8 {
                for resolution in 0..4 {
                    let mut d = device();
                    write(&mut d, 0, (gain << 4) | hall);
                    write(&mut d, 2, 14 | (resolution * 21 << 5));
                    for (field, value) in [(29, 123.4), (30, -234.5), (31, 345.6)] {
                        assert!(d.set(field, value));
                    }
                    let response = measure(&mut d);
                    assert_eq!(response[0], 3);
                    for axis in 0..3 {
                        let raw =
                            u16::from_be_bytes([response[3 + axis * 2], response[4 + axis * 2]]);
                        let signed = match resolution {
                            0 | 1 => raw as i16 as f64,
                            2 => f64::from(raw) - 32768.,
                            _ => f64::from(raw) - 16384.,
                        };
                        let expected = [123.4, -234.5, 345.6][axis];
                        assert!(
                            (signed * d.scale(axis) - expected).abs()
                                <= d.scale(axis) / 2. + 0.00001
                        );
                        assert_eq!(d.value(29 + axis as u32), signed * d.scale(axis));
                    }
                }
            }
        }
        let mut d = device();
        write(&mut d, 2, 14);
        d.set(29, 50000.);
        measure(&mut d);
        assert_eq!(d.value(29), 4915.05);
        for resolution in [2, 3] {
            write(&mut d, 2, 14 | (resolution << 5));
            d.set(29, 50000.);
            measure(&mut d);
            assert_eq!(d.value(29), 13200.);
            d.set(29, -50000.);
            measure(&mut d);
            assert_eq!(d.value(29), -13200.);
        }
        write(&mut d, 2, 14);
        d.set(29, 150.);
        write(&mut d, 1, 0x400);
        for axis in 0..3 {
            write(&mut d, 4 + axis, 32768 + if axis == 0 { 10 } else { 0 });
        }
        let r = measure(&mut d);
        assert_eq!(u16::from_be_bytes([r[3], r[4]]), 33758);
        assert_eq!(d.value(29), 148.5);
        write(&mut d, 3, 0x8080);
        d.set(0, 45.);
        let r = measure(&mut d);
        assert_eq!(u16::from_be_bytes([r[3], r[4]]), 33867);
        assert_eq!(d.value(29), 164.85);
        write(&mut d, 2, 14 | (2 << 5));
        assert_eq!(
            command(&mut d, &[0x3f])[0] & 0x10,
            0x10,
            "TCMP resolution2 is invalid"
        );
    }
    #[test]
    fn mlx90393_burst_wake_threshold_relative_reference_exit_and_channel_mask() {
        let mut d = device();
        write(&mut d, 2, 14);
        write(&mut d, 1, (2 << 6) | 2);
        d.set(29, 30.);
        assert_eq!(command(&mut d, &[0x10])[0], 0x80);
        assert_eq!(d.axes, 2);
        let first = d.next.unwrap();
        time(&mut d, first);
        assert_eq!(d.generation(), 1);
        assert_eq!(d.value(29), 30.);
        assert!(d.value(30).is_nan());
        assert_eq!(command(&mut d, &[0x60, 0, 0, 0])[0] & 0x10, 0x10);
        assert_eq!(d.memory[0], 0x7c);
        assert_eq!(d.next, Some(first + 43087));
        d.set(29, 60.);
        time(&mut d, first + 43086);
        assert_eq!(d.value(29), 30.);
        time(&mut d, first + 43087);
        assert_eq!(d.value(29), 60.);
        let mixed = command(&mut d, &[0x4f]);
        assert_eq!(
            &mixed[..5],
            &[0x80, 1, 0x90, 0xff, 0xff],
            "only converted X is serialized despite requested TXYZ"
        );
        let accessible = d.accessible_at;
        time(&mut d, accessible);
        command(&mut d, &[0x80]);
        d.set(29, 90.);
        time(&mut d, 200000);
        assert_eq!(d.value(29), 60.);
        write(&mut d, 1, 0);
        write(&mut d, 7, 20);
        assert_eq!(command(&mut d, &[0x22])[0], 0x40);
        {
            let at = d.next.unwrap();
            time(&mut d, at);
        }
        assert_eq!(
            command(&mut d, &[0x42])[0] & 0x10,
            0x10,
            "first WOC sample establishes reference"
        );
        d.set(29, 91.5);
        {
            let at = d.next.unwrap();
            time(&mut d, at);
        }
        assert_eq!(command(&mut d, &[0x42])[0] & 0x10, 0x10);
        d.set(29, 94.5);
        {
            let at = d.next.unwrap();
            time(&mut d, at);
        }
        assert_eq!(command(&mut d, &[0x42])[0], 0x40);
        let accessible = d.accessible_at;
        time(&mut d, accessible);
        command(&mut d, &[0x80]);
        write(&mut d, 1, 0x1000);
        command(&mut d, &[0x22]);
        {
            let at = d.next.unwrap();
            time(&mut d, at);
        }
        for value in [96., 97.5, 99.] {
            d.set(29, value);
            {
                let at = d.next.unwrap();
                time(&mut d, at);
            }
            assert_eq!(
                command(&mut d, &[0x42])[0] & 0x10,
                0x10,
                "relative threshold follows previous sample"
            );
        }
    }
    #[test]
    fn mlx90393_shared_i2c_command_adapter_preserves_response_and_address_straps() {
        let clock = Arc::new(AtomicU64::new(0));
        for address in 0x0c..=0x0f {
            let config = SensorConfig {
                id: 0,
                sda: 4,
                scl: 5,
                address,
                model: 47,
                shunt_milliohms: 0,
            };
            assert!(config.valid());
            let sensor = Arc::new(Mutex::new(Sensor::new(config, clock.clone(), 1_000_000)));
            let mut bus = SensorI2c::new(sensor);
            assert!(bus.matches_address(address, address, false));
            assert!(!bus.matches_address(address, 0x1c, false));
            assert_eq!(bus.pins(), Some((4, 5)));
            assert!(bus.start(false));
            assert!(bus.write(0x50));
            assert!(bus.write(0));
            bus.stop();
            assert!(bus.start(true));
            assert_eq!([bus.read(), bus.read(), bus.read()], [0, 0, 0x7c]);
            bus.stop();
        }
    }
}

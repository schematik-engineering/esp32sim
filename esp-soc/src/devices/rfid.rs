use crate::SpiPins;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct RfidConfig {
    pub id: u8,
    pub sclk: u8,
    pub mosi: u8,
    pub miso: u8,
    pub cs: u8,
    pub reset: u8,
}
impl RfidConfig {
    pub fn valid(&self) -> bool {
        let pins = [self.sclk, self.mosi, self.miso, self.cs, self.reset];
        pins.iter().enumerate().all(|(i, &pin)| {
            (pin < 49 || (i == 4 && pin == 255)) && (pin == 255 || !pins[..i].contains(&pin))
        })
    }
}
struct Card {
    uid: Vec<u8>,
    halted: bool,
}
enum Pending {
    Response(Vec<u8>),
    Timeout,
    Error(u8),
}
/// MFRC522 SPI register/FIFO interface and ISO14443A single-card selection.
/// Register addresses and command framing follow NXP MFRC522 Rev.3.9 sections8/9.
pub struct Rfid {
    pub config: RfidConfig,
    registers: [u8; 64],
    fifo: VecDeque<u8>,
    card: Option<Card>,
    selected: bool,
    reset_high: bool,
    spi_address: Option<(u8, bool)>,
    pending: Option<(u64, Pending)>,
    cycle: u64,
    hz: u64,
}
impl Rfid {
    pub fn new(config: RfidConfig, hz: u64) -> Self {
        let mut reader = Self {
            config,
            registers: [0; 64],
            fifo: VecDeque::new(),
            card: None,
            selected: false,
            reset_high: true,
            spi_address: None,
            pending: None,
            cycle: 0,
            hz,
        };
        reader.reset();
        reader
    }
    fn reset(&mut self) {
        self.registers = [0; 64];
        self.registers[0x11] = 0x3f;
        self.registers[0x15] = 0x00;
        self.registers[0x24] = 0x26;
        self.registers[0x37] = 0x92;
        self.fifo.clear();
        self.pending = None;
        if let Some(card) = &mut self.card {
            card.halted = false;
        }
    }
    pub fn card(&mut self, uid: &[u8]) -> bool {
        if ![0, 4, 7, 10].contains(&uid.len()) {
            return false;
        }
        self.card = if uid.is_empty() {
            None
        } else {
            Some(Card {
                uid: uid.to_vec(),
                halted: false,
            })
        };
        true
    }
    pub fn gpio(&mut self, pin: u8, high: bool) {
        if pin == self.config.cs {
            self.selected = !high;
            if high {
                self.spi_address = None;
            }
        }
        if pin == self.config.reset && high != self.reset_high {
            self.reset_high = high;
            self.reset();
            self.spi_address = None;
        }
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.pending.as_ref().map(|(cycle, _)| *cycle)
    }
    pub fn advance_to(&mut self, cycle: u64) {
        self.cycle = cycle;
        if self
            .pending
            .as_ref()
            .is_some_and(|(deadline, _)| *deadline <= cycle)
        {
            match self.pending.take().unwrap().1 {
                Pending::Response(bytes) => {
                    self.fifo = bytes.into();
                    self.registers[0x0c] &= !7;
                    self.registers[4] |= 0x30;
                }
                Pending::Timeout => {
                    self.fifo.clear();
                    self.registers[4] |= 1;
                }
                Pending::Error(error) => {
                    self.fifo.clear();
                    self.registers[6] |= error;
                    self.registers[4] |= 0x12;
                }
            }
        }
    }
    fn read(&mut self, address: u8) -> u8 {
        match address {
            9 => self.fifo.pop_front().unwrap_or(0),
            10 => self.fifo.len() as u8,
            _ => self.registers[address as usize],
        }
    }
    fn write(&mut self, address: u8, value: u8) {
        match address {
            4 | 5 => {
                if value & 0x80 != 0 {
                    self.registers[address as usize] |= value & 0x7f;
                } else {
                    self.registers[address as usize] &= !(value & 0x7f);
                }
            }
            9 => {
                if self.fifo.len() < 64 {
                    self.fifo.push_back(value);
                } else {
                    self.registers[6] |= 0x10;
                }
            }
            10 => {
                if value & 0x80 != 0 {
                    self.fifo.clear();
                    self.registers[6] &= !0x10;
                }
            }
            1 => {
                self.registers[1] = value;
                match value & 0x0f {
                    0 => self.pending = None,
                    3 => {
                        let seed = [0, 0x6363, 0xa671, 0xffff][(self.registers[0x11] & 3) as usize];
                        let bytes: Vec<u8> = self.fifo.drain(..).collect();
                        let crc = crc_seed(&bytes, seed);
                        self.registers[0x22] = crc as u8;
                        self.registers[0x21] = (crc >> 8) as u8;
                        self.registers[5] |= 4;
                    }
                    0x0f => self.reset(),
                    _ => {}
                }
            }
            0x0d => {
                self.registers[0x0d] = value & 0x7f;
                if value & 0x80 != 0 && self.registers[1] & 0x0f == 0x0c {
                    self.transceive();
                }
            }
            0x37 | 6 => {}
            _ => self.registers[address as usize] = value,
        }
    }
    fn timeout_cycles(&self) -> u64 {
        let prescaler = (((self.registers[0x2a] & 15) as u64) << 8) | self.registers[0x2b] as u64;
        let reload = ((self.registers[0x2c] as u64) << 8) | self.registers[0x2d] as u64;
        (self.hz.saturating_mul((prescaler * 2 + 1) * reload) / 13_560_000).max(1)
    }
    fn transceive(&mut self) {
        let frame: Vec<u8> = self.fifo.drain(..).collect();
        self.registers[6] = 0;
        let response = self.radio(&frame);
        let delay = if matches!(response, Pending::Timeout) {
            self.timeout_cycles()
        } else {
            (self.hz / 10000).max(1)
        };
        self.pending = Some((self.cycle.saturating_add(delay), response));
    }
    fn radio(&mut self, frame: &[u8]) -> Pending {
        if self.registers[0x14] & 3 != 3 {
            return Pending::Timeout;
        }
        let Some(card) = &mut self.card else {
            return Pending::Timeout;
        };
        if frame.len() == 1 && self.registers[0x0d] & 7 == 7 && [0x26, 0x52].contains(&frame[0]) {
            if card.halted && frame[0] != 0x52 {
                return Pending::Timeout;
            }
            card.halted = false;
            return Pending::Response(vec![
                match card.uid.len() {
                    7 => 0x44,
                    10 => 0x84,
                    _ => 4,
                },
                0,
            ]);
        }
        if card.halted {
            return Pending::Timeout;
        }
        if frame.len() == 4 && frame[..2] == [0x50, 0] {
            if valid_crc(frame) {
                card.halted = true;
            }
            return Pending::Timeout;
        }
        if frame.len() < 2 {
            return Pending::Error(1);
        }
        let level = match frame[0] {
            0x93 => 0,
            0x95 => 1,
            0x97 => 2,
            _ => return Pending::Timeout,
        };
        let offset = level * 3;
        if offset + 4 > card.uid.len() {
            return Pending::Timeout;
        }
        let cascade = card.uid.len() > offset + 4;
        let mut block = if cascade {
            vec![
                0x88,
                card.uid[offset],
                card.uid[offset + 1],
                card.uid[offset + 2],
            ]
        } else {
            card.uid[offset..offset + 4].to_vec()
        };
        block.push(block.iter().fold(0, |bcc, byte| bcc ^ byte));
        match frame[1] {
            0x20 => Pending::Response(block),
            0x70 if frame.len() == 9 && frame[2..7] == block && valid_crc(frame) => {
                let sak = if cascade { 4 } else { 8 };
                let crc = crc_a(&[sak]);
                Pending::Response(vec![sak, crc as u8, (crc >> 8) as u8])
            }
            _ => Pending::Error(1),
        }
    }
    pub fn spi(&mut self, pins: SpiPins, tx: &[u8], rx_len: usize) -> Option<Vec<u8>> {
        let hardware_cs = pins.cs & (1u64 << self.config.cs) != 0;
        if !self.reset_high
            || !(self.selected || hardware_cs)
            || pins.sclk & (1u64 << self.config.sclk) == 0
            || pins.mosi & (1u64 << self.config.mosi) == 0
        {
            return None;
        }
        let mut rx = vec![0xff; rx_len];
        for (index, &byte) in tx.iter().enumerate() {
            let result = match self.spi_address {
                None => {
                    self.spi_address = Some(((byte >> 1) & 0x3f, byte & 0x80 != 0));
                    0
                }
                Some((address, true)) => {
                    let value = self.read(address);
                    self.spi_address = if byte & 0x80 != 0 {
                        Some(((byte >> 1) & 0x3f, true))
                    } else {
                        None
                    };
                    value
                }
                Some((address, false)) => {
                    self.write(address, byte);
                    0
                }
            };
            if pins.miso == Some(self.config.miso) && index < rx_len {
                rx[index] = result;
            }
        }
        if hardware_cs && !self.selected {
            self.spi_address = None;
        }
        Some(rx)
    }
}
fn crc_a(bytes: &[u8]) -> u16 {
    crc_seed(bytes, 0x6363)
}
fn crc_seed(bytes: &[u8], mut crc: u16) -> u16 {
    for byte in bytes {
        crc ^= *byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}
fn valid_crc(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && crc_a(&bytes[..bytes.len() - 2])
            == u16::from_le_bytes([bytes[bytes.len() - 2], bytes[bytes.len() - 1]])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reader() -> Rfid {
        Rfid::new(
            RfidConfig {
                id: 0,
                sclk: 1,
                mosi: 2,
                miso: 3,
                cs: 4,
                reset: 5,
            },
            240_000_000,
        )
    }
    fn route() -> SpiPins {
        SpiPins {
            sclk: 2,
            mosi: 4,
            miso: Some(3),
            cs: 0,
        }
    }
    #[test]
    fn spi_fifo_crc_and_physical_select() {
        let mut r = reader();
        assert!(r.spi(route(), &[0xee, 0], 2).is_none());
        r.gpio(4, false);
        assert_eq!(r.spi(route(), &[0xee, 0], 2).unwrap(), [0, 0x92]);
        r.gpio(4, true);
        r.gpio(4, false);
        r.spi(route(), &[0x12, 0x50, 0], 3);
        r.gpio(4, true);
        r.write(0x11, 0x3d);
        r.write(1, 3);
        assert_eq!((r.read(0x22), r.read(0x21)), (0x57, 0xcd));
        let wrong = SpiPins {
            miso: Some(6),
            ..route()
        };
        r.gpio(4, false);
        assert_eq!(r.spi(wrong, &[0xee, 0], 2).unwrap(), [255, 255]);
        r.gpio(5, false);
        assert!(r.spi(route(), &[0xee, 0], 2).is_none());
    }
    #[test]
    fn card_selection_cascade_crc_halt_and_wakeup() {
        for length in [4, 7, 10] {
            let mut r = reader();
            r.registers[0x14] = 3;
            r.registers[0x0d] = 7;
            let uid: Vec<u8> = (0..length).map(|n| 0x10 + n).collect();
            assert!(r.card(&uid));
            assert!(matches!(r.radio(&[0x26]), Pending::Response(_)));
            r.registers[0x0d] = 0;
            for level in 0..=(length - 4) / 3 {
                let command = 0x93 + level * 2;
                let Pending::Response(block) = r.radio(&[command, 0x20]) else {
                    panic!()
                };
                assert_eq!(block.len(), 5);
                assert_eq!(block.iter().fold(0, |x, b| x ^ b), 0);
                let mut select = vec![command, 0x70];
                select.extend(block);
                let crc = crc_a(&select);
                select.extend(crc.to_le_bytes());
                let Pending::Response(sak) = r.radio(&select) else {
                    panic!()
                };
                assert!(valid_crc(&sak));
                assert_eq!(sak[0], if level == (length - 4) / 3 { 8 } else { 4 });
                select[8] ^= 1;
                assert!(matches!(r.radio(&select), Pending::Error(_)));
            }
            assert!(matches!(r.radio(&[0x50, 0, 0x57, 0xcd]), Pending::Timeout));
            r.registers[0x0d] = 7;
            assert!(matches!(r.radio(&[0x26]), Pending::Timeout));
            assert!(matches!(r.radio(&[0x52]), Pending::Response(_)));
            assert!(r.card(&[]));
            assert!(matches!(r.radio(&[0x26]), Pending::Timeout));
        }
    }
    #[test]
    fn timeout_uses_configured_timer_and_fifo_is_bounded() {
        let mut r = reader();
        r.registers[0x2a] = 0x80;
        r.registers[0x2b] = 0xa9;
        r.registers[0x2c] = 3;
        r.registers[0x2d] = 0xe8;
        for n in 0..65 {
            r.write(9, n);
        }
        assert_eq!(r.read(10), 64);
        assert_eq!(r.read(6) & 0x10, 0x10);
        r.write(10, 0x80);
        r.write(9, 0x26);
        r.write(1, 0x0c);
        r.write(0x0d, 0x87);
        let deadline = r.next_deadline().unwrap();
        assert_eq!(deadline, 6_000_000);
        r.advance_to(deadline - 1);
        assert_eq!(r.read(4) & 1, 0);
        r.advance_to(deadline);
        assert_eq!(r.read(4) & 1, 1);
    }
}

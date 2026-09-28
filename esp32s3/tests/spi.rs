use emu_core::Bus;
use esp_soc::{BoardModel, SpiPins};
use std::sync::{Arc, Mutex};
struct Capture(Arc<Mutex<Vec<(u8, SpiPins, Vec<u8>)>>>);
impl BoardModel for Capture {
    fn name(&self) -> &'static str {
        "spi-capture"
    }
    fn spi_transfer_pins(&mut self, host: u8, pins: SpiPins, tx: &[u8], n: usize) -> Vec<u8> {
        self.0.lock().unwrap().push((host, pins, tx.to_vec()));
        tx.iter()
            .copied()
            .chain(std::iter::repeat(0xff))
            .take(n)
            .collect()
    }
}
#[test]
fn spi3_polling_and_gdma_use_second_host_routes_and_trigger() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut b = esp32s3::bus::SocBus::new(1 << 20, 0, [0; 6]);
    b.board = Box::new(Capture(seen.clone()));
    for pin in [0, 1, 3] {
        b.write32(0x60009004 + pin * 4, 1 << 12 | 1 << 9).unwrap();
    }
    b.write32(0x60004554, 66).unwrap();
    b.write32(0x60004558, 68).unwrap();
    b.write32(0x60004154 + 67 * 4, 1 << 7 | 3).unwrap();
    b.write32(0x60025010, 1 << 27 | 1 << 28).unwrap();
    b.write32(0x6002501c, 31).unwrap();
    b.write32(0x60025098, 0xaaff005a).unwrap();
    b.write32(0x60025000, 1 << 24).unwrap();
    assert_eq!(b.read32(0x60025098).unwrap(), 0xaaff005a);
    let descriptor = 0x3fc89000;
    for (offset, value) in [
        (0, 1 << 31 | 1 << 30 | 4 << 12 | 4),
        (4, descriptor + 16),
        (8, 0),
        (16, 0x12345678),
    ] {
        b.write32(descriptor + offset, value).unwrap();
    }
    b.write32(0x60025010, 1 << 27).unwrap();
    b.write32(0x60025030, 1 << 28).unwrap();
    b.write32(0x6003f060, 1 << 2).unwrap();
    b.write32(0x6003f0a8, 1).unwrap();
    b.write32(0x6003f080, (descriptor & 0xfffff) | 1 << 21)
        .unwrap();
    b.write32(0x60025000, 1 << 24).unwrap();
    assert_eq!(b.periph.spi3.transfers, 2);
    assert_eq!(b.periph.spi2.transfers, 0);
    assert_eq!(b.read32(descriptor).unwrap() >> 31, 0);
    let records = seen.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        (
            records[0].0,
            records[0].1.sclk,
            records[0].1.mosi,
            records[0].1.miso
        ),
        (3, 1, 2, Some(3))
    );
    assert_eq!(records[1].2, [0x78, 0x56, 0x34, 0x12]);
}

use esp_soc::board::BoardEdge;

use esp32::bus::SocBus;
use esp_soc::{BoardModel, SocBus as _};
use xtensa_lx7::bus::Bus;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Probe { edges: Vec<BoardEdge>, writes: Vec<u64>, uart: Vec<(esp_soc::uart::UartRoute, u8)> }
struct Board(Arc<Mutex<Probe>>);
impl BoardModel for Board {
    fn name(&self) -> &'static str { "host-test" }
    fn gpio_output_at(&mut self, cycle: u64, _: &[(u8, bool)], _: u64, _: u64) {
        let mut p = self.0.lock().unwrap(); p.writes.push(cycle);
        p.edges.push(BoardEdge { cycle, pin: 5, level: true });
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> { std::mem::take(&mut self.0.lock().unwrap().edges) }
    fn uart_tx(&mut self, route: esp_soc::uart::UartRoute, byte: u8) { self.0.lock().unwrap().uart.push((route, byte)); }
    fn uart_rx(&mut self) -> Vec<esp_soc::uart::UartInput> { vec![esp_soc::uart::UartInput { pin: 5, baud: 115200, data: vec![42] }] }
}
fn bus() -> SocBus { SocBus::new(4 << 20, [0; 6]) }
#[test]
fn timestamped_feedback_and_released_pull_survive_reboot() {
    let mut b = bus(); let probe = Arc::new(Mutex::new(Probe::default())); b.board = Box::new(Board(probe.clone()));
    b.write32(0x3ff4_9048, (2 << 12) | (1 << 9)).unwrap();
    b.write32(0x3ff4_906c, (2 << 12) | (1 << 9) | (1 << 8)).unwrap();
    b.begin_execution(100, 20); b.note_instruction(27);
    b.write32(0x3ff4_4024, 1 << 4).unwrap(); b.write32(0x3ff4_4008, 1 << 4).unwrap();
    assert_eq!(probe.lock().unwrap().writes.last(), Some(&107));
    assert_ne!(b.read32(0x3ff4_403c).unwrap() & (1 << 5), 0);
    b.gpio_set_input(5, false); b.reboot([0; 6]); assert_eq!(b.gpio_input() & (1 << 5), 0);
    b.write32(0x3ff4_906c, (2 << 12) | (1 << 9) | (1 << 8)).unwrap();
    b.gpio_release_input(5); assert_ne!(b.gpio_input() & (1 << 5), 0); assert!(b.gpio_state(5).unwrap().pull_up);
}
#[test]
fn uart_apb_ahb_and_pin_receive() {
    let mut b = bus(); let probe = Arc::new(Mutex::new(Probe::default())); b.board = Box::new(Board(probe.clone()));
    b.write32(0x3ff4_9048, (2 << 12) | (1 << 9)).unwrap();
    b.write32(0x3ff4_906c, (2 << 12) | (1 << 9)).unwrap();
    b.write32(0x3ff4_4540, 17).unwrap(); b.write32(0x3ff4_4024, 1 << 4).unwrap();
    b.write32(0x3ff4_4174, 0x80 | 5).unwrap(); // UART1 RX signal 17
    b.write32(0x3ff5_0014, 694).unwrap();
    b.write32(0x3ff5_0000, 65).unwrap(); b.write32(0x6001_0000, 66).unwrap();
    let p = probe.lock().unwrap(); assert_eq!(p.uart.len(), 2); assert!(p.uart[0].0.transmits_on(4)); assert_eq!(p.uart[1].1, 66); drop(p);
    b.tick(1); assert_eq!(b.read32(0x6001_0000), Ok(42));
    b.write32(0x3ff5_0014, 8333).unwrap();
    assert!(b.periph.uart_route(1).matches_baud(9600));
    b.write32(0x3ff5_0020, 0).unwrap();
    b.write32(0x3ff5_0014, 104).unwrap();
    assert!(b.periph.uart_route(1).matches_baud(9600));
}
#[test]
fn relay_bounds_drain_and_reset() {
    let mut b = bus(); b.set_ethernet_relay(true).unwrap(); assert!(b.receive_ethernet_frame(&[0; 14]).is_err());
    b.periph.wifi.ap = Some(esp_soc::wifi::VirtualAp::new(esp_soc::wifi::ApConfig::parse("ssid=test").unwrap(), false));
    assert!(b.receive_ethernet_frame(&[0; 13]).is_err());
    for _ in 0..64 { b.receive_ethernet_frame(&[0; 14]).unwrap(); }
    assert!(b.receive_ethernet_frame(&[0; 14]).is_err());
    b.periph.wifi.eth_tx.push(vec![7; 14]); assert_eq!(b.take_ethernet_frames(), vec![vec![7; 14]]);
    b.reboot([0; 6]); assert!(b.periph.wifi.relay); assert!(b.periph.wifi.eth_rx.is_empty());
}
#[test]
fn raw_adc_and_stream_are_observed() {
    let mut b = bus(); assert!(b.adc_set_raw(34, 1234)); assert!(!b.adc_set_raw(31, 1));
    b.write32(0x3ff4_8800, 1 << 28).unwrap();
    b.write32(0x3ff4_8854, (1 << 31) | (1 << 25) | (1 << 18) | (1 << 17)).unwrap();
    assert_eq!(b.read32(0x3ff4_8854).unwrap() & 0xffff, 1234);
    assert_eq!(b.adc_observation(34).unwrap().generation, 1);
    assert_eq!(b.adc_observation(34).unwrap().raw, 1234);
}
#[test]
fn i2s_native_dma_packs_pcm_and_raises_eof() {
    let mut b = bus(); let base = 0x3ff4_f000;
    b.write32(0x3ff0_00c0, 1 << 4).unwrap();
    for (address, value) in [(0x3ffb_0100, 0x8000_0008), (0x3ffb_0104, 0x3ffb_0200), (0x3ffb_0108, 0)] { b.write32(address, value).unwrap(); }
    b.i2s_input(0).unwrap().push(&[[123, -456], [789, -123]]);
    b.write32(base + 0xac, 25 | (1 << 20)).unwrap();
    b.write32(base + 0xb0, (25 << 6) | (16 << 18)).unwrap();
    b.write32(base + 0x14, 1 << 9).unwrap();
    b.write32(base + 0x34, (1 << 29) | 0xb0100).unwrap(); b.write32(base + 8, 1 << 5).unwrap();
    b.tick(60_000);
    assert_eq!(b.read32(0x3ffb_0200), Ok(123 | ((-456i16 as u16 as u32) << 16)));
    assert_eq!(b.read32(base + 0x3c), Ok(0x3ffb_0100));
    assert_ne!(b.read32(base + 0x10).unwrap() & (1 << 9), 0);
    assert_eq!(b.read32(0x3ffb_0100).unwrap() >> 31, 0);
}

#[test]
fn physical_spi_excludes_released_and_high_selects() {
    struct SpiBoard(Arc<Mutex<Vec<esp_soc::board::SpiPins>>>);
    impl BoardModel for SpiBoard {
        fn name(&self) -> &'static str { "spi-pins" }
        fn spi_transfer_pins(&mut self, _: u8, pins: esp_soc::board::SpiPins, _: &[u8], len: usize) -> Vec<u8> {
            self.0.lock().unwrap().push(pins); vec![0xa5; len]
        }
    }
    let mut b = bus(); let routes = Arc::new(Mutex::new(Vec::new())); b.board = Box::new(SpiBoard(routes.clone()));
    // SPI2 native clock/MOSI GPIO14/13, software CS GPIO4.
    for addr in [0x3ff4_9030, 0x3ff4_9038] { b.write32(addr, (1 << 12) | (1 << 9)).unwrap(); }
    b.write32(0x3ff4_9048, (2 << 12) | (1 << 9)).unwrap();
    b.write32(0x3ff4_4024, 1 << 4).unwrap();
    for high in [false, true] {
        b.write32(if high { 0x3ff4_4008 } else { 0x3ff4_400c }, 1 << 4).unwrap();
        b.write32(0x3ff6_401c, (1 << 27) | (1 << 28) | 1).unwrap();
        b.write32(0x3ff6_4028, 7).unwrap(); b.write32(0x3ff6_402c, 7).unwrap(); b.write32(0x3ff6_4000, 1 << 18).unwrap();
        let p = *routes.lock().unwrap().last().unwrap();
        assert_ne!(p.sclk & (1 << 14), 0); assert_ne!(p.mosi & (1 << 13), 0);
        assert_eq!(p.cs & (1 << 4) != 0, !high);
    }
}

use emu_core::Bus;
use esp_soc::board::{BoardEdge, BoardModel, SpiPins};

#[derive(Default)]
struct Feedback {
    now: u64,
    edges: Vec<BoardEdge>,
}
impl BoardModel for Feedback {
    fn name(&self) -> &'static str {
        "same-cycle-feedback"
    }
    fn gpio_output_at(&mut self, cycle: u64, changes: &[(u8, bool)], _: u64, _: u64) {
        for &(pin, level) in changes {
            if pin == 4 {
                self.edges.push(BoardEdge {
                    cycle,
                    pin: 5,
                    level: !level,
                });
                self.edges.push(BoardEdge {
                    cycle: cycle + 100,
                    pin: 5,
                    level,
                });
            }
        }
        self.edges.sort_by_key(|e| e.cycle);
    }
    fn advance_to(&mut self, cycle: u64) {
        self.now = cycle;
    }
    fn next_deadline(&self) -> Option<u64> {
        self.edges.first().map(|e| e.cycle)
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> {
        let n = self.edges.partition_point(|e| e.cycle <= self.now);
        self.edges.drain(..n).collect()
    }
}

#[test]
fn gpio_reads_deliver_same_cycle_feedback_and_preserve_future_edges() {
    let mut m = machine!();
    let b = &mut m.bus;
    b.board = Box::<Feedback>::default();
    b.gpio_events = Some(Vec::new());
    b.write32(GPIO + 0x24, 1 << 4).unwrap();
    b.write32(GPIO + 0x74 + 4 * 5, 3 << 7 | 1 << 13).unwrap();
    b.write32(GPIO + 8, 1 << 4).unwrap();
    b.irq_dirty = false;
    assert_eq!(b.read32(GPIO + 0x3c).unwrap() & 32, 0);
    assert!(b.irq_dirty);
    assert_ne!(b.periph.gpio.status & 32, 0);
    assert_eq!(b.cycles, 0);
    if CPU_HZ == 160_000_000 {
        assert_eq!(b.read8(GPIO + 0x3c).unwrap() & 32, 0);
    }
    b.tick(99);
    assert_eq!(b.read32(GPIO + 0x3c).unwrap() & 32, 0);
    if CPU_HZ == 160_000_000 {
        assert_eq!(b.read16(GPIO + 0x3c).unwrap() & 32, 0);
    }
    b.tick(1);
    assert_ne!(b.read32(GPIO + 0x3c).unwrap() & 32, 0);
    b.write32(GPIO + 0xc, 1 << 4).unwrap();
    assert_ne!(b.read32(GPIO + 0x3c).unwrap() & 32, 0);
    b.tick(100);
    assert_eq!(b.read32(GPIO + 0x3c).unwrap() & 32, 0);
    let inputs: Vec<_> = b
        .gpio_events
        .as_ref()
        .unwrap()
        .iter()
        .copied()
        .filter(|&(_, pin, _)| pin == 5)
        .collect();
    assert_eq!(
        inputs,
        [
            (0, 5, false),
            (100, 5, true),
            (100, 5, true),
            (200, 5, false)
        ]
    );
}

#[derive(Default)]
struct SpiBoard {
    bit: u32,
    edges: Vec<BoardEdge>,
}
impl BoardModel for SpiBoard {
    fn name(&self) -> &'static str {
        "spi-feedback"
    }
    fn gpio_output_at(&mut self, cycle: u64, changes: &[(u8, bool)], enabled: u64, out: u64) {
        if enabled & (1 << 10) == 0 || out & (1 << 10) != 0 {
            self.bit = 0;
            return;
        }
        for &(pin, high) in changes {
            if pin == 6 && high {
                self.edges.push(BoardEdge {
                    cycle,
                    pin: 2,
                    level: 0x8123_a55au32 & (1 << (31 - self.bit % 32)) != 0,
                });
                self.bit += 1;
            }
        }
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> {
        std::mem::take(&mut self.edges)
    }
    fn uses_spi_pins(&self) -> bool {
        true
    }
    fn spi_transfer_pins(&mut self, _: u8, pins: SpiPins, tx: &[u8], n: usize) -> Vec<u8> {
        println!("SPI route={pins:?}");
        assert_eq!(tx, [0x5a]);
        let selected = pins.sclk & (1 << 6) != 0
            && pins.mosi & (1 << 7) != 0
            && pins.miso == Some(2)
            && pins.cs & (1 << 10) != 0;
        vec![if selected { 0xa5 } else { 0xff }; n]
    }
}

#[test]
#[ignore = "requires ESP32SIM_FEEDBACK_BUILD and ESP32SIM_ROM"]
fn external_arduino_gpio_feedback_and_spi_select() {
    let build = std::path::PathBuf::from(
        std::env::var_os("ESP32SIM_FEEDBACK_BUILD")
            .expect("ESP32SIM_FEEDBACK_BUILD must name the Arduino build directory"),
    );
    let rom = std::env::var_os("ESP32SIM_ROM").expect("ESP32SIM_ROM must name the chip ROM ELF");
    let mut m = machine!();
    m.bus.board = Box::<SpiBoard>::default();
    m.console.capture = true;
    m.console.mask = 2;
    m.load_rom(&std::fs::read(rom).expect("read ROM ELF"))
        .unwrap();
    m.write_flash(
        0,
        &std::fs::read(build.join("firmware.factory.bin")).expect("read firmware.factory.bin"),
    )
    .unwrap();
    m.boot_rom();
    m.max_cycles = CPU_HZ;
    let stop = m.run(u64::MAX);
    let console = String::from_utf8_lossy(&m.console.all);
    println!(
        "{console}\nstop={stop:?} cycles={} instructions={}",
        m.bus.cycles,
        m.insns()
    );
    assert!(console.contains("SOFT=8123a55a"), "{console}");
    assert!(
        console.contains("SPI right=a5 wrong=ff released=ff"),
        "{console}"
    );
}

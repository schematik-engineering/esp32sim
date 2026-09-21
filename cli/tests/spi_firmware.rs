use emu_core::Core;
use esp_soc::{BoardModel, Machine, Soc, SocBus, SpiPins};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<(u8, SpiPins, Vec<u8>)>>>);
impl BoardModel for Capture {
    fn name(&self) -> &'static str {
        "spi-loopback"
    }
    fn spi_transfer_pins(&mut self, host: u8, pins: SpiPins, tx: &[u8], rx_len: usize) -> Vec<u8> {
        self.0.lock().unwrap().push((host, pins, tx.to_vec()));
        if pins.sclk != 0 && pins.mosi != 0 && pins.miso.is_some() {
            tx.iter()
                .copied()
                .chain(std::iter::repeat(0xff))
                .take(rx_len)
                .collect()
        } else {
            vec![0xff; rx_len]
        }
    }
}
fn run<S: Soc>(mut m: Machine<S>, root: &Path, roms: &Path) {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    m.bus.set_board(Box::new(capture.clone()));
    m.console.capture = true;
    m.load_rom(&std::fs::read(roms.join(S::ROM_ELF)).unwrap())
        .unwrap();
    let chip = root.join(S::NAME);
    m.add_symbols(&std::fs::read(chip.join(".pio/build/serial/firmware.elf")).unwrap()).unwrap();
    let manifest = std::fs::read_to_string(chip.join("spi-flash.txt")).unwrap();
    for line in manifest.lines() {
        let (offset, path) = line.split_once(' ').unwrap();
        m.bus
            .write_flash(offset.parse().unwrap(), &std::fs::read(path).unwrap())
            .unwrap();
    }
    m.boot_rom();
    let started = std::time::Instant::now();
    for _ in 0..3000 {
        assert!(
            started.elapsed().as_secs() < 120,
            "SPI wall deadline: {}",
            String::from_utf8_lossy(&m.console.all)
        );
        match m.run(500_000) {
            esp_soc::Stop::MaxInsns | esp_soc::Stop::Halted => {}
            esp_soc::Stop::SwReset if m.reboots < 3 => {
                m.reboot();
            }
            stop => panic!(
                "SPI firmware stopped: {stop:?} {}",
                String::from_utf8_lossy(&m.console.all)
            ),
        }
        let serial = String::from_utf8_lossy(&m.console.all);
        assert!(
            !serial.contains("assert failed") && !serial.contains("Guru Meditation"),
            "{serial}"
        );
        if serial.contains("SPI_DONE") {
            assert!(!serial.contains("FAIL"), "{serial}");
            let records = capture.0.lock().unwrap();
            assert_eq!(
                records.len(),
                if S::NAME == "esp32s3" { 6 } else { 4 },
                "{serial}\n{records:?}"
            );
            for (index, line) in serial
                .lines()
                .filter(|l| l.starts_with("POLL ") || l.starts_with("DMA "))
                .enumerate()
            {
                let fields: Vec<_> = line.split_whitespace().collect();
                let (host, pins, tx) = &records[index];
                assert_eq!(*host, if fields[1] == "spi3" { 3 } else { 2 });
                if fields[0] == "POLL" {
                    let clk: u8 = fields[2].parse().unwrap();
                    let mosi: u8 = fields[3].parse().unwrap();
                    let miso: u8 = fields[4].parse().unwrap();
                    assert_eq!(
                        (pins.sclk, pins.mosi, pins.miso),
                        (1 << clk, 1 << mosi, Some(miso)),
                        "{line}"
                    );
                    assert_eq!(*tx, [0x5a, 0, 1, 0x7f, 0x80, 0xff, 0xa5, 0x33]);
                } else {
                    let clk: u8 = fields[2].parse().unwrap();
                    let mosi: u8 = fields[3].parse().unwrap();
                    assert_eq!(
                        (pins.sclk, pins.mosi, pins.miso),
                        (1 << clk, 1 << mosi, None)
                    );
                    assert_eq!(
                        *tx,
                        (0..1024).map(|i| (i * 37 + 11) as u8).collect::<Vec<_>>()
                    );
                }
            }
            println!("{}: {}", S::NAME, serial);
            return;
        }
    }
    for core in &m.cores { eprintln!("PC {:#x} {:?}",core.pc(),m.symbols.range(..=core.pc()).next_back()); }
    eprintln!("SPI records {:?}",capture.0.lock().unwrap().iter().map(|(h,p,t)|(*h,*p,t.len())).collect::<Vec<_>>());
    panic!(
        "{} SPI firmware deadline: {}",
        S::NAME,
        String::from_utf8_lossy(&m.console.all)
    );
}
#[test]
#[ignore = "requires hardware-compiled fixture and Espressif ROMs: ESP32SIM_SPI_FIRMWARE, ESP32SIM_ROM_DIR"]
fn unchanged_arduino_polling_and_idf_dma() {
    let root = std::env::var("ESP32SIM_SPI_FIRMWARE").unwrap();
    let roms = std::env::var("ESP32SIM_ROM_DIR").unwrap();
    let chip = std::env::var("ESP32SIM_SPI_CHIP").expect("fixture chip");
    assert!(["esp32s3", "esp32c3", "esp32c6"].contains(&chip.as_str()));
    let flash = std::env::var("ESP32SIM_FLASH_BYTES")
        .expect("compiled flash capacity")
        .parse()
        .unwrap();
    let psram = std::env::var("ESP32SIM_PSRAM_BYTES")
        .expect("compiled PSRAM capacity")
        .parse()
        .unwrap();
    if chip == "esp32s3" {
        run(
            Machine::<esp32s3::soc::S3>::new(
                [2, 0, 0, 0, 0, 1],
                esp32s3::bus::SocBus::new(flash, psram, [2, 0, 0, 0, 0, 1]),
            ),
            Path::new(&root),
            Path::new(&roms),
        );
    }
    if chip == "esp32c3" {
        run(
            esp32c3::soc::machine([2, 0, 0, 0, 0, 1], flash),
            Path::new(&root),
            Path::new(&roms),
        );
    }
    if chip == "esp32c6" {
        run(
            esp32c6::soc::machine([2, 0, 0, 0, 0, 1], flash),
            Path::new(&root),
            Path::new(&roms),
        );
    }
}

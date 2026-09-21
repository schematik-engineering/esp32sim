use emu_core::{Bus, Fault};
use esp32c3::bus::{SocBus, RTC_SLOW_HIGH};

fn bus() -> SocBus { SocBus::new(1, [0; 6]) }

#[test]
fn reads_ending_at_buffer_boundary_use_their_full_width() {
    let mut b = bus();
    let end = b.rtc_slow.len();
    b.rtc_slow[end - 4..].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    assert_eq!(b.read8(RTC_SLOW_HIGH - 1), Ok(0x44));
    assert_eq!(b.read16(RTC_SLOW_HIGH - 2), Ok(0x4433));
    assert_eq!(b.read32(RTC_SLOW_HIGH - 4), Ok(0x4433_2211));
}

#[test]
fn reads_crossing_buffer_boundary_are_unmapped() {
    let mut b = bus();
    assert_eq!(b.read16(RTC_SLOW_HIGH - 1), Err(Fault::Unmapped));
    assert_eq!(b.read32(RTC_SLOW_HIGH - 3), Err(Fault::Unmapped));
}

#[test]
fn spi2_polling_routes_physical_pins_and_returns_miso_before_next_read() {
    use esp_soc::{BoardModel, SpiPins};
    use std::sync::{Arc,Mutex};
    struct Loopback(Arc<Mutex<Vec<SpiPins>>>);
    impl BoardModel for Loopback {
        fn name(&self)->&'static str { "loopback" }
        fn spi_transfer_pins(&mut self,host:u8,pins:SpiPins,tx:&[u8],n:usize)->Vec<u8> {
            assert_eq!(host,2); self.0.lock().unwrap().push(pins);
            if pins.sclk==1 && pins.mosi==2 && pins.miso==Some(3) {tx[..n].to_vec()} else {vec![0xff;n]}
        }
    }
    let seen=Arc::new(Mutex::new(Vec::new())); let mut b=bus(); b.board=Box::new(Loopback(seen.clone()));
    for pin in [0,1,3] { b.write32(0x60009004+pin*4,1<<12|1<<9).unwrap(); }
    b.write32(0x60004554,63).unwrap(); b.write32(0x60004558,65).unwrap();
    b.write32(0x60004154+64*4,1<<6|3).unwrap();
    b.write32(0x60024010,1<<27|1<<28).unwrap(); b.write32(0x6002401c,31).unwrap();
    b.write32(0x60024098,0x80ff005a).unwrap(); b.write32(0x60024000,1<<24).unwrap();
    assert_eq!(b.read32(0x60024098).unwrap(),0x80ff005a);
    b.write32(0x60009008,0).unwrap();
    b.write32(0x60024098,0x12345678).unwrap(); b.write32(0x60024000,1<<24).unwrap();
    assert_eq!(b.read32(0x60024098).unwrap(),0xffffffff);
    assert_eq!(seen.lock().unwrap().len(),2);
    assert_ne!(b.read32(0x6002403c).unwrap()&(1<<12),0);
}

#[test]
fn spi2_waits_for_physical_gdma_channel_and_reports_descriptor_error() {
    let mut b=bus();
    let descriptor=0x3fc80100;
    b.write32(descriptor,1<<31|1<<30|4<<12|4).unwrap();
    b.write32(descriptor+4,0x3fc80200).unwrap(); b.write32(descriptor+8,0).unwrap();
    b.write32(0x3fc80200,0x80ff005a).unwrap();
    b.write32(0x60024010,1<<27).unwrap(); b.write32(0x6002401c,31).unwrap();
    b.write32(0x60024030,1<<28).unwrap(); b.write32(0x60024000,1<<24).unwrap();
    assert_eq!(b.periph.spi2.transfers,0);
    b.write32(0x6003f0d0,1<<2).unwrap();
    b.write32(0x6003f100,0).unwrap();
    b.write32(0x6003f0e0,(descriptor&0xfffff)|1<<21).unwrap();
    assert_eq!(b.periph.spi2.transfers,1);
    assert_eq!(b.read32(descriptor).unwrap()>>31,0);
    assert_eq!(b.periph.gdma.gdma.out[0].int_raw,11);
    b.write32(0x6003f00c,u32::MAX).unwrap();
    b.write32(0x60024000,1<<24).unwrap();
    b.write32(0x6003f0e0,(descriptor&0xfffff)|1<<21).unwrap();
    assert_eq!(b.periph.spi2.transfers,1);
    assert_eq!(b.periph.gdma.gdma.out[0].int_raw,4);
}

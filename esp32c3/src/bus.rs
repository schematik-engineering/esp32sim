//! ESP32-C3 memory map.
//!
//! Simpler than the S3's: one core, no PSRAM, an 8 MB cache window per bus and a flat 128-entry
//! MMU. SRAM1 is dual-mapped (IRAM `0x4038_0000` and DRAM `0x3FC8_0000` are the same bytes);
//! SRAM0 below it is the instruction cache's, reachable only from the instruction bus.

use crate::periph::{Peripherals, PERIPH_BASE, PERIPH_END};
use riscv_rv32::bus::{Bus, Fault};

pub const SRAM_SIZE: usize = 400 * 1024;
pub const IRAM_LOW: u32 = 0x4037_C000;
pub const IRAM_HIGH: u32 = 0x403E_0000;
pub const DRAM_LOW: u32 = 0x3FC8_0000;
pub const DRAM_HIGH: u32 = 0x3FCE_0000;
/// SRAM1 starts 16 KiB into the buffer: SRAM0 in front of it is instruction-bus only.
pub const DRAM_IN_SRAM: usize = 0x4000;
pub const IROM_MASK_LOW: u32 = 0x4000_0000;
pub const IROM_MASK_HIGH: u32 = 0x4006_0000;
pub const DROM_MASK_LOW: u32 = 0x3FF0_0000;
pub const DROM_MASK_HIGH: u32 = 0x3FF2_0000;
pub const RTC_SLOW_LOW: u32 = 0x5000_0000;
pub const RTC_SLOW_HIGH: u32 = 0x5000_2000;
pub const DBUS_LOW: u32 = 0x3C00_0000;
pub const DBUS_HIGH: u32 = 0x3C80_0000;
pub const IBUS_LOW: u32 = 0x4200_0000;
pub const IBUS_HIGH: u32 = 0x4280_0000;
pub const MMU_TABLE: u32 = 0x600C_5000;
pub const MMU_ENTRIES: usize = 128;
/// bit 8 marks an entry invalid; bits 7:0 are the 64 KiB flash page
pub const MMU_INVALID: u32 = 1 << 8;
pub const PAGE: u32 = 0x1_0000;

pub struct SocBus {
    pub sram: Vec<u8>,
    pub irom: Vec<u8>,
    pub drom: Vec<u8>,
    pub rtc_slow: Vec<u8>,
    pub flash: Vec<u8>,
    pub mmu: [u32; MMU_ENTRIES],
    pub periph: Peripherals,
    /// a bare module: nothing on the pins
    pub board: esp_soc::Board,
    pub cycles: u64,
    pub last_fault: Option<(u32, bool)>,
    /// a peripheral write may have moved an interrupt line: re-derive before the next instruction
    pub irq_dirty: bool,
    /// GPIO edges for observers, while one wants them: (cycle, pin, level)
    pub gpio_events: Option<Vec<(u64, u8, bool)>>,
    pub debug: esp_soc::DebugFlags,
}

impl SocBus {
    pub fn sync_board_inputs(&mut self) {
        self.board.advance_to(self.cycles);
        let old=self.periph.gpio.input;
        let old_status=self.periph.gpio.status;
        for edge in self.board.take_edges() {
            if let Some(events)=&mut self.gpio_events {events.push((edge.cycle,edge.pin,edge.level));}
            self.periph.gpio.set_input(edge.pin,edge.level);
        }
        for (pin,level) in self.board.input_levels() {self.periph.gpio.set_input(pin,level);}
        for pin in self.board.released_inputs() {
            let before=self.periph.gpio.input;
            self.periph.gpio.release_input(pin);
            if before!=self.periph.gpio.input {if let Some(events)=&mut self.gpio_events {events.push((self.cycles,pin,self.periph.gpio.level(pin)));}}
        }
        self.irq_dirty |= old!=self.periph.gpio.input || old_status!=self.periph.gpio.status;
    }

    pub fn new(flash_size: usize, mac: [u8; 6]) -> Self {
        SocBus {
            sram: vec![0; SRAM_SIZE],
            irom: vec![0; (IROM_MASK_HIGH - IROM_MASK_LOW) as usize],
            drom: vec![0; (DROM_MASK_HIGH - DROM_MASK_LOW) as usize],
            rtc_slow: vec![0; (RTC_SLOW_HIGH - RTC_SLOW_LOW) as usize],
            flash: vec![0xff; flash_size],
            mmu: [MMU_INVALID; MMU_ENTRIES],
            periph: Peripherals::new(mac), board: Box::new(esp_soc::NoBoard),
            cycles: 0, last_fault: None, irq_dirty: true, gpio_events: None, debug: Default::default(),
        }
    }

    /// Resolve to (buffer, offset, writable). Cache windows go through the MMU.
    fn resolve(&mut self, addr: u32) -> Option<(&mut Vec<u8>, usize, bool)> {
        match addr {
            DRAM_LOW..=0x3FCD_FFFF => Some((&mut self.sram, (addr - DRAM_LOW) as usize + DRAM_IN_SRAM, true)),
            IRAM_LOW..=0x403D_FFFF => Some((&mut self.sram, (addr - IRAM_LOW) as usize, true)),
            IROM_MASK_LOW..=0x4005_FFFF => Some((&mut self.irom, (addr - IROM_MASK_LOW) as usize, false)),
            DROM_MASK_LOW..=0x3FF1_FFFF => Some((&mut self.drom, (addr - DROM_MASK_LOW) as usize, false)),
            RTC_SLOW_LOW..=0x5000_1FFF => Some((&mut self.rtc_slow, (addr - RTC_SLOW_LOW) as usize, true)),
            DBUS_LOW..=0x3C7F_FFFF | IBUS_LOW..=0x427F_FFFF => {
                // both buses index one flat table; software keeps their page ranges disjoint
                let entry = self.mmu[((addr & 0x7F_FFFF) >> 16) as usize];
                if entry & MMU_INVALID != 0 { return None; }
                let off = (entry & 0xff) as usize * PAGE as usize + (addr & 0xffff) as usize;
                if off < self.flash.len() { Some((&mut self.flash, off, false)) } else { None }
            }
            _ => None,
        }
    }

    #[inline]
    fn is_periph(addr: u32) -> bool { (PERIPH_BASE..PERIPH_END).contains(&addr) }

    fn periph_read(&mut self, addr: u32, size: u32) -> u32 {
        if (MMU_TABLE..MMU_TABLE + (MMU_ENTRIES as u32) * 4).contains(&addr) {
            return self.mmu[((addr - MMU_TABLE) >> 2) as usize];
        }
        let w = self.periph.read32(addr & !3);
        match size { 1 => (w >> ((addr & 3) * 8)) & 0xff, 2 => (w >> ((addr & 2) * 8)) & 0xffff, _ => w }
    }

    fn periph_write(&mut self, addr: u32, v: u32, size: u32) {
        if (MMU_TABLE..MMU_TABLE + (MMU_ENTRIES as u32) * 4).contains(&addr) {
            self.mmu[((addr - MMU_TABLE) >> 2) as usize] = v & 0x1ff;
            return;
        }
        let a = addr & !3;
        let v = match size {
            4 => v,
            1 => { let old = self.periph.read32(a); let sh = (addr & 3) * 8; (old & !(0xff << sh)) | ((v & 0xff) << sh) }
            _ => { let old = self.periph.read32(a); let sh = (addr & 2) * 8; (old & !(0xffff << sh)) | ((v & 0xffff) << sh) }
        };
        let old_drive=(self.periph.gpio.enable,self.periph.gpio.out);
        self.periph.write32(a, v);
        if old_drive!=(self.periph.gpio.enable,self.periph.gpio.out) {
            self.board.gpio_drive(self.cycles,self.periph.gpio.enable,self.periph.gpio.out);
            self.sync_board_inputs();
        }
        // A SPI flash command must complete before the guest can read its result: firmware kicks
        // the command and polls/reads the data registers a few instructions later, well inside one
        // scheduling quantum. Running it at the quantum boundary instead loses the race and the
        // read returns zeros — which is exactly how `E memspi: no response` showed up on a
        // non-power-on boot while a power-on boot happened to survive it.
        if self.periph.spi_exec { self.run_spi(); }
        self.spi2_dma_tx();
        self.deliver_spi2_transfer();
        self.irq_dirty = true;
    }

    fn spi2_dma_tx(&mut self) {
        let Some(bits) = self.periph.spi2.dma_tx_pending else { return };
        let Some(ch) = self.periph.gdma.gdma.out_channel_for(0) else { return };
        match esp_periph::spi_dma::transmit(&mut self.sram[DRAM_IN_SRAM..], DRAM_LOW, &mut self.periph.gdma.gdma.out[ch], bits) {
            Ok(data) => self.periph.spi2.complete_dma_tx(&data),
            Err(_) => self.periph.spi2.fail_dma_tx(),
        }
        self.irq_dirty = true;
    }

    fn deliver_spi2_transfer(&mut self) {
        let Some(transfer) = self.periph.spi2.take_transfer() else { return };
        let changes = std::mem::take(&mut self.periph.gpio.changes);
        if let Some(events) = &mut self.gpio_events { events.extend(changes.iter().map(|&(pin, level)| (self.cycles, pin, level))); }
        self.board.gpio_changes(&changes);
        let pins = esp_soc::spi::SpiLayout::C3.pins(&self.periph.gpio, &self.periph.spi2);
        let rx = self.board.spi_transfer_pins(2, pins, &transfer.tx, transfer.rx_len);
        self.periph.spi2.finish_transfer(transfer, &rx);
        self.irq_dirty = true;
    }

    /// Execute a pending SPI1 command against the flash image.
    fn run_spi(&mut self) {
        self.periph.spi_exec = false;
        let mut no_psram = Vec::new();
        self.periph.spi1.execute(&mut self.flash, &mut no_psram);
        self.periph.spi1.dirty.clear();
    }

    /// Write straight into flash (image loaders, not the guest).
    pub fn write_flash(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
        if offset + data.len() > self.flash.len() { return Err("flash image too large".into()); }
        self.flash[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn sha_dma_step(&mut self) {
        let Some(out_ch) = self.periph.gdma.gdma.out_channel_for(7) else { return; };
        self.periph.sha.dma_pending = false;
        let bs = self.periph.sha.block_bytes();
        let Some(want) = (self.periph.sha.block_num as usize).checked_mul(bs) else { return; };
        if want == 0 || want > 1_048_576 { return; }
        let mut input = Vec::with_capacity(want);
        let mut descriptors = Vec::new();
        let mut desc = self.periph.gdma.gdma.out[out_ch].desc;
        let mut visited = std::collections::HashSet::new();
        while desc != 0 && input.len() < want {
            if !visited.insert(desc) || visited.len() > 512 { return; }
            let Ok(dw0) = self.read32(desc) else { return; };
            let len = ((dw0 >> 12) & 0xfff) as usize;
            if dw0 & (1 << 31) == 0 || len == 0 || len > (dw0 & 0xfff) as usize { return; }
            let (Ok(buf), Ok(next)) = (self.read32(desc + 4), self.read32(desc + 8)) else { return; };
            for i in 0..len.min(want - input.len()) {
                let Some(address) = buf.checked_add(i as u32) else { return; };
                let Ok(byte) = self.read8(address) else { return; };
                input.push(byte);
            }
            descriptors.push((desc, dw0));
            if dw0 & (1 << 30) != 0 { break; }
            desc = next;
        }
        if input.len() != want { return; }
        let mut first = self.periph.sha.dma_first;
        for block in input.chunks_exact(bs) {
            self.periph.sha.hash_block(block, first);
            first = false;
        }
        for (desc, dw0) in descriptors {
            let _ = self.write32(desc, dw0 & !(1 << 31));
            self.periph.gdma.gdma.out[out_ch].int_raw |= 1;
            if dw0 & (1 << 30) != 0 {
                self.periph.gdma.gdma.out[out_ch].int_raw |= 1 << 1;
                self.periph.gdma.gdma.out[out_ch].eof_desc = desc;
            }
        }
        self.periph.sha.busy = false;
        self.irq_dirty = true;
    }

    fn aes_dma_step(&mut self) {
        let (Some(out_ch), Some(in_ch)) = (self.periph.gdma.gdma.out_channel_for(6), self.periph.gdma.gdma.in_channel_for(6)) else {
            return;
        };
        self.periph.aes.dma_pending = false;
        // gather input
        let mut input = Vec::new();
        let mut desc = self.periph.gdma.gdma.out[out_ch].desc;
        let mut visited = std::collections::HashSet::new();
        while desc != 0 {
            if !visited.insert(desc) || input.len() > 1_048_576 { return; }
            let dw0 = self.read32(desc).unwrap_or(0);
            let (len, buf, next) = (((dw0 >> 12) & 0xfff) as usize, self.read32(desc + 4).unwrap_or(0), self.read32(desc + 8).unwrap_or(0));
            for i in 0..len { input.push(self.read8(buf + i as u32).unwrap_or(0)); }
            let eof = dw0 & (1 << 30) != 0;
            let _ = self.write32(desc, dw0 & !(1 << 31));                       // hand the descriptor back
            if eof { self.periph.gdma.gdma.out[out_ch].int_raw |= (1 << 0) | (1 << 1); self.periph.gdma.gdma.out[out_ch].eof_desc = desc; break; }
            desc = next;
        }
        if self.debug.has("aes") {
            eprintln!("[aes] dma block_mode={} num_blocks={} mode={} bytes={}", self.periph.aes.block_mode, self.periph.aes.num_blocks, self.periph.aes.mode, input.len());
        }
        // transform (ECB and CBC cover what the crypto libraries ask for here)
        let key = self.periph.aes.key_bytes();
        let decrypt = self.periph.aes.decrypting();
        let block_mode = self.periph.aes.block_mode;
        let mut iv = [0u8; 16];
        for (i, w) in self.periph.aes.iv.iter().enumerate() { iv[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes()); }
        let mut output = Vec::with_capacity(input.len());
        for chunk in input.chunks(16) {
            let mut b = [0u8; 16];
            b[..chunk.len()].copy_from_slice(chunk);
            let cipher_in = b;
            let o = match block_mode {
                1 => {                                                          // CBC
                    if !decrypt { for i in 0..16 { b[i] ^= iv[i]; } }
                    let mut o = esp_periph::crypto::aes_block(&key, &b, decrypt);
                    if decrypt { for i in 0..16 { o[i] ^= iv[i]; } iv = cipher_in; } else { iv = o; }
                    o
                }
                2 => {                                                          // OFB: keystream feeds itself
                    let ks = esp_periph::crypto::aes_block(&key, &iv, false);
                    iv = ks;
                    let mut o = [0u8; 16];
                    for i in 0..16 { o[i] = b[i] ^ ks[i]; }
                    o
                }
                3 => {                                                          // CTR: encrypt the counter, then bump it
                    let ks = esp_periph::crypto::aes_block(&key, &iv, false);
                    let mut o = [0u8; 16];
                    for i in 0..16 { o[i] = b[i] ^ ks[i]; }
                    for i in (0..16).rev() { iv[i] = iv[i].wrapping_add(1); if iv[i] != 0 { break; } }
                    o
                }
                _ => esp_periph::crypto::aes_block(&key, &b, decrypt),                // ECB
            };
            output.extend_from_slice(&o);
            self.periph.aes.blocks += 1;
        }
        for (i, w) in iv.chunks(4).enumerate() { self.periph.aes.iv[i] = u32::from_le_bytes([w[0], w[1], w[2], w[3]]); }
        // scatter the result
        let mut pos = 0usize;
        let mut desc = self.periph.gdma.gdma.inp[in_ch].desc;
        visited.clear();
        while desc != 0 && pos < output.len() {
            if !visited.insert(desc) { return; }
            let dw0 = self.read32(desc).unwrap_or(0);
            let (size, buf, next) = ((dw0 & 0xfff) as usize, self.read32(desc + 4).unwrap_or(0), self.read32(desc + 8).unwrap_or(0));
            let n = size.min(output.len() - pos);
            for i in 0..n { let _ = self.write8(buf + i as u32, output[pos + i]); }
            pos += n;
            let ndw0 = (dw0 & !(0xfff << 12) & !(1 << 31)) | ((n as u32) << 12) | (1 << 30);
            let _ = self.write32(desc, ndw0);
            self.periph.gdma.gdma.inp[in_ch].eof_desc = desc;
            self.periph.gdma.gdma.inp[in_ch].int_raw |= (1 << 0) | (1 << 1);
            if next == 0 { break; }
            desc = next;
        }
        self.periph.aes.state = 2;                                              // DONE
        self.periph.aes.int_raw |= 1;
        self.irq_dirty = true;
    }

    /// WiFi MAC transmit: fetch the queued frames from their DMA descriptors and complete them.
    fn wifi_tx_step(&mut self) {
        let pending = std::mem::take(&mut self.periph.wifi.tx_pending);
        for (slot, desc) in pending {
            let dw0 = self.read32(desc).unwrap_or(0); let pkt = self.read32(desc + 4).unwrap_or(0);
            let len = ((dw0 >> 12) & 0xfff) as usize;
            let mut frame = Vec::with_capacity(len);
            for i in 0..len { frame.push(self.read8(pkt + i as u32).unwrap_or(0)); }
            if self.periph.wifi.log || self.debug.has("wifi-frames") { eprintln!("[wifi] TX slot {} desc {:#010x} pkt {:#010x} {}", slot, desc, pkt, crate::wifi::describe(&frame)); }
            self.periph.wifi.tx_done(slot);
            self.irq_dirty = true;
            let now_us = self.cycles / (crate::periph::CPU_HZ / 1_000_000);
            if let Some(ap) = &mut self.periph.wifi.ap {
                if let Some(data) = ap.on_station_tx(&frame, now_us) {
                    if let Some(eth) = crate::wifi::data_to_eth(&data) {
                        if eth.len() <= 1518 && self.periph.wifi.eth_tx.len() < 64 { self.periph.wifi.eth_tx.push(eth); }
                    }
                }
            }
        }
    }

    /// The virtual air: beacons/responses from the AP and frames from the network backend land in the RX ring.
    fn wifi_air_step(&mut self) {
        let now_us = self.cycles / (crate::periph::CPU_HZ / 1_000_000);
        // The blob's RX path only *indicates* a frame up the 802.11 stack while the descriptor ring is
        // shallow; with several filled descriptors pending it switches to batch block-recycle and drops
        // them. So hold off until the previously delivered descriptor has been recycled by software
        // (has_data cleared) — that is what a real radio sees at low traffic — and never deliver two
        // frames closer than a frame's airtime.
        if now_us.wrapping_sub(self.periph.wifi.last_rx_us) < 400 { return; }
        // ... but if software stops recycling altogether, don't stall the air forever: after 50 ms
        // the frame is dropped, exactly as a real ring would overflow.
        let busy = { let d = self.periph.wifi.last_rx_desc; d != 0 && self.read32(d).unwrap_or(0) & (1 << 30) != 0 };
        if busy && now_us.wrapping_sub(self.periph.wifi.last_rx_us) < 50_000 { return; }
        let mut due = { let ap = self.periph.wifi.ap.as_mut().unwrap(); ap.step(now_us) };
        let eth_in = std::mem::take(&mut self.periph.wifi.eth_rx);
        for e in eth_in { if let Some(f) = self.periph.wifi.ap.as_mut().unwrap().data_from_ds(&e) { due.push(crate::wifi::AirFrame { at_us: now_us, frame: f }); } }
        if due.is_empty() { return; }
        // management responses (auth, assoc, probe) go before beacons: a connect exchange must not be
        // crowded out by beacon traffic
        due.sort_by_key(|a| (crate::wifi::is_beacon(&a.frame), a.at_us));
        let first = due.remove(0);
        self.wifi_rx_deliver(&first.frame, now_us);
        self.periph.wifi.last_rx_us = now_us;
        if let Some(ap) = &mut self.periph.wifi.ap { for a in due { ap.queue.push(a); } }
    }

    /// Write one received frame into the next RX descriptor (rx_ctrl header + frame + FCS) and raise the RX event.
    #[allow(clippy::identity_op, reason = "rx_state zero remains visible in the packed descriptor layout")]
    fn wifi_rx_deliver(&mut self, frame: &[u8], now_us: u64) {
        if self.periph.wifi.rx_next == 0 { self.periph.wifi.rx_dropped += 1; return; }
        let desc = self.periph.wifi.rx_next | esp_periph::DMA_ADDR_BASE;
        let dw0 = self.read32(desc).unwrap_or(0); let buf = self.read32(desc + 4).unwrap_or(0); let next = self.read32(desc + 8).unwrap_or(0);
        let size = (dw0 & 0xfff) as usize;
        let total = 48 + frame.len() + 4;
        if dw0 & (3 << 30) != 1 << 31 || buf == 0 || size < total { self.periph.wifi.rx_dropped += 1; return; }
        let (chan, log) = { let ap = self.periph.wifi.ap.as_ref().unwrap(); (ap.cfg.channel as u32, ap.log) };
        let mut b = Vec::with_capacity(total);
        // rx_ctrl word 0 (silicon: a real broadcast beacon reads 0x111b20ad — bit 28 set, signed rssi in the low
        // byte). The MAC has already address-filtered, so every delivered frame is "for us"; use the same flags
        // for unicast and broadcast (an invented "filter_match" nibble made the blob discard unicast frames).
        // filter-match nibble (silicon: broadcast beacon reads bit 28). A frame the hardware accepted because
        // addr1 is our unicast MAC must carry the unicast-match bit (29), not the broadcast bit (28), or
        // wDev_IndicateFrame drops it as "not for me".
        let bcast = frame.len() >= 5 && frame[4] & 1 == 1;
        // filter-match nibble: bit 28 is the "accepted by the address filter" bit the blob's RX path
        // requires (silicon: a broadcast beacon reads 0x111b20ad); unicast frames add bit 29.
        let fm = if bcast { 1u32 << 28 } else { (1u32 << 28) | (1u32 << 29) };
        let w0: u32 = fm | (0xd8u32 & 0xff);   // rssi -40 dBm, 1 Mbps, legacy
        let w2: u32 = (chan << 16) | (chan << 20);                                        // channel, secondary
        let w5: u32 = 0xa6;                                                                // noise floor -90
        let w11: u32 = ((frame.len() + 4) as u32 & 0xfff) | (0 << 24);                    // sig_len (incl. FCS), rx_state OK
        for w in [w0, 0, w2, now_us as u32, 0, w5, 0, 0, 0, 0, 0, w11] { b.extend_from_slice(&w.to_le_bytes()); }
        b.extend_from_slice(frame); b.extend_from_slice(&crate::wifi::fcs(frame).to_le_bytes());
        let mut i = 0usize;
        while i + 4 <= b.len() { let v = u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]); let _ = self.write32(buf + i as u32, v); i += 4; }
        while i < b.len() { let _ = self.write8(buf + i as u32, b[i]); i += 1; }
        let ndw0 = (dw0 & !(0xfff << 12)) | ((total as u32) << 12) | (1 << 30) | (1 << 31);   // length; owner AND has_data set (verified on silicon 2026-08-25: dw0=0xc0..)
        let _ = self.write32(desc, ndw0);
        let w = &mut self.periph.wifi;
        w.rx_last = (desc & 0xf_ffff) | (1 << 24); w.rx_next = next & 0xf_ffff; w.last_rx_desc = desc; w.rx_frames += 1; w.events |= (1 << 14) | (1 << 24);   // RX data (wDev_ProcessFiq tests 0x1004000)   // registers hold masked descriptor addrs; rx_last has a 0x01 prefix (silicon)
        if log { let d = crate::wifi::describe(frame); if d.contains("auth")||d.contains("assoc") { eprintln!("[wifi] RX AUTH/ASSOC -> desc {:#010x} buf {:#010x} {}", desc, buf, d); } else { eprintln!("[wifi] RX -> desc {:#010x} {}", desc, d); } }
        self.irq_dirty = true;
    }

    pub fn load_bytes(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        for (i, b) in data.iter().enumerate() {
            let a = addr.wrapping_add(i as u32);
            match self.resolve(a) {
                Some((buf, off, _)) if off < buf.len() => buf[off] = *b,
                _ => return Err(format!("load: address {:#010x} not mapped", a)),
            }
        }
        Ok(())
    }

    /// Run the SPI1 controller if the guest just kicked it, then advance device time.
    fn devices(&mut self, cycles: u32) {
        if self.periph.spi_exec { self.run_spi(); }
        self.spi2_dma_tx();
        self.deliver_spi2_transfer();
        self.periph.tick(cycles as u64);
        self.sync_board_inputs();
        let bytes = self.periph.i2s.rx_data(cycles as u64, &self.periph.gpio, 15, 6, 0x1ff);
        if let Some(ch) = self.periph.gdma.gdma.in_channel_for(3) {
            let mut channel = self.periph.gdma.gdma.inp[ch];
            let eof = self.periph.i2s.rx_eof_bytes();
            esp_periph::i2s::receive_dma(self, &mut channel, &bytes, eof);
            self.irq_dirty |= channel.int_raw != self.periph.gdma.gdma.inp[ch].int_raw;
            self.periph.gdma.gdma.inp[ch] = channel;
        }

        if self.periph.aes.dma_pending { self.aes_dma_step(); }
        if self.periph.sha.dma_pending { self.sha_dma_step(); }
        if !self.periph.wifi.tx_pending.is_empty() { self.wifi_tx_step(); }
        if self.periph.wifi.ap.is_some() { self.wifi_air_step(); }
        if !self.periph.wifi.relay {
            if let Some(net) = &mut self.periph.wifi.net {
                let now_us = self.cycles / (crate::periph::CPU_HZ / 1_000_000);
                let out = std::mem::take(&mut self.periph.wifi.eth_tx);
                // Frames from the station are handled the moment they are sent, but reading the host
                // sockets means syscalls: doing that every scheduling round costs more than emulating
                // the CPU. NET_POLL_US is well under any timeout the guest's TCP stack cares about.
                const NET_POLL_US: u64 = 500;
                let due = now_us.wrapping_sub(self.periph.wifi.net_polled_us) >= NET_POLL_US;
                if !out.is_empty() || due {
                    if due { self.periph.wifi.net_polled_us = now_us; }
                    let mut replies = Vec::new();
                    for e in out { replies.extend(net.handle(&e, now_us)); }
                    replies.extend(net.poll(now_us));
                    self.periph.wifi.eth_rx.extend(replies);
                }
            }
        }

        let changes = std::mem::take(&mut self.periph.gpio.changes);
        if let Some(events) = &mut self.gpio_events {
            events.extend(changes.iter().map(|&(pin, level)| (self.cycles, pin, level)));
        }
        self.board.gpio_changes(&changes);
        for (channel, bits) in std::mem::take(&mut self.periph.rmt.rmt.done) {
            for pin in self.periph.gpio.pins_for_signal(51 + channel as u32) {
                self.board.rmt_frame(pin, &bits);
            }
            self.irq_dirty = true;
        }
    }
}

macro_rules! rd {
    ($self:ident, $addr:expr, $n:expr, $conv:expr) => {{
        let addr = $addr;
        match $self.resolve(addr) {
            Some((b, o, _)) if b.len().saturating_sub(o) >= $n => Ok($conv(&b[o..o + $n])),
            _ => { $self.last_fault = Some((addr, false)); Err(Fault::Unmapped) }
        }
    }};
}

impl Bus for SocBus {
    fn read8(&mut self, addr: u32) -> Result<u8, Fault> {
        if Self::is_periph(addr) { return Ok(self.periph_read(addr, 1) as u8); }
        rd!(self, addr, 1, |b: &[u8]| b[0])
    }
    fn read16(&mut self, addr: u32) -> Result<u16, Fault> {
        if Self::is_periph(addr) { return Ok(self.periph_read(addr, 2) as u16); }
        rd!(self, addr, 2, |b: &[u8]| u16::from_le_bytes(b.try_into().unwrap()))
    }
    fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        if Self::is_periph(addr) { return Ok(self.periph_read(addr, 4)); }
        rd!(self, addr, 4, |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault> {
        if Self::is_periph(addr) { self.periph_write(addr, v as u32, 1); return Ok(()); }
        match self.resolve(addr) {
            Some((b, o, true)) if o < b.len() => { b[o] = v; Ok(()) }
            _ => { self.last_fault = Some((addr, true)); Err(Fault::Prohibited) }
        }
    }
    fn write16(&mut self, addr: u32, v: u16) -> Result<(), Fault> {
        if Self::is_periph(addr) { self.periph_write(addr, v as u32, 2); return Ok(()); }
        match self.resolve(addr) {
            Some((b, o, true)) if o + 2 <= b.len() => { b[o..o + 2].copy_from_slice(&v.to_le_bytes()); Ok(()) }
            _ => { self.last_fault = Some((addr, true)); Err(Fault::Prohibited) }
        }
    }
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), Fault> {
        if Self::is_periph(addr) { self.periph_write(addr, v, 4); return Ok(()); }
        match self.resolve(addr) {
            Some((b, o, true)) if o + 4 <= b.len() => { b[o..o + 4].copy_from_slice(&v.to_le_bytes()); Ok(()) }
            _ => { self.last_fault = Some((addr, true)); Err(Fault::Prohibited) }
        }
    }
    fn fetch(&mut self, pc: u32) -> Result<[u8; 4], Fault> {
        match self.resolve(pc) {
            Some((b, o, _)) if o < b.len() => {
                let mut r = [0u8; 4];
                for i in 0..4 { if o + i < b.len() { r[i] = b[o + i]; } }
                Ok(r)
            }
            _ => { self.last_fault = Some((pc, false)); Err(Fault::Unmapped) }
        }
    }
    fn tick(&mut self, cycles: u32) -> u32 {
        self.cycles += cycles as u64;
        self.devices(cycles);
        1
    }
    #[inline(always)]
    fn note_pc(&mut self, pc: u32) { self.periph.misc.cur_pc = pc; }
    /// a peripheral write may have moved a line: the core's run stops so the machine re-derives it
    #[inline(always)]
    fn block_break(&self) -> bool { self.irq_dirty }
}

#[cfg(test)]
mod gpio_delivery_tests {
    use super::*;
    #[test]
    fn gpio_edges_are_consumed_and_observed_each_device_slice() {
        let mut bus = SocBus::new(4 << 20, [0; 6]);
        bus.gpio_events = Some(Vec::new());
        bus.periph.gpio.write(0x24, 1 << 4);
        bus.periph.gpio.write(0x8, 1 << 4);
        bus.devices(1);
        assert!(bus.periph.gpio.changes.is_empty());
        assert_eq!(bus.gpio_events.as_deref(), Some(&[(0, 4, true)][..]));
        bus.devices(1);
        assert_eq!(bus.gpio_events.as_ref().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod aes_dma_tests {
    use super::*;
    #[test]
    fn aes_dma_encrypts_the_fips_block_and_returns_descriptor_ownership() {
        let mut bus = SocBus::new(4 << 20, [0; 6]);
        let tx = DRAM_LOW; let rx = DRAM_LOW + 0x20;
        let input = DRAM_LOW + 0x40; let output = DRAM_LOW + 0x60;
        for (i, word) in [0x03020100, 0x07060504, 0x0b0a0908, 0x0f0e0d0c].iter().enumerate() { bus.periph.aes.key[i] = *word; }
        for (i, word) in [0x33221100, 0x77665544, 0xbbaa9988, 0xffeeddcc].iter().enumerate() { bus.write32(input + i as u32 * 4, *word).unwrap(); }
        for (addr, size, buf) in [(tx, (3 << 30) | (16 << 12) | 16, input), (rx, (1 << 31) | 16, output)] {
            bus.write32(addr, size).unwrap(); bus.write32(addr + 4, buf).unwrap(); bus.write32(addr + 8, 0).unwrap();
        }
        bus.write32(0x6003f100, 6).unwrap();
        bus.write32(0x6003f0a0, 6).unwrap();
        bus.write32(0x6003f0e0, (1 << 21) | (tx & 0xfffff)).unwrap();
        bus.write32(0x6003f080, (1 << 22) | (rx & 0xfffff)).unwrap();
        bus.write32(0x6003f008, (1 << 4) | (1 << 1)).unwrap();
        bus.periph.aes.dma_pending = true;
        bus.tick(1);
        assert_eq!(bus.periph.aes.state, 2);
        assert_eq!(bus.read32(0x6003f004).unwrap(), (1 << 4) | (1 << 1));
        bus.write32(0x6003f00c, 1 << 4).unwrap();
        assert_eq!(bus.read32(0x6003f004).unwrap(), 1 << 1);
        assert_eq!(bus.read32(tx).unwrap() >> 31, 0);
        assert_eq!(bus.read32(rx).unwrap() >> 31, 0);
        let words: Vec<_> = (0..4).map(|i| bus.read32(output + i * 4).unwrap()).collect();
        assert_eq!(words, [0xd8e0c469, 0x30047b6a, 0x80b7cdd8, 0x5ac5b470]);
    }
}

#[cfg(test)]
mod sha_dma_tests {
    use super::*;
    use emu_core::Bus;

    fn fixture() -> SocBus {
        let mut bus = SocBus::new(4 << 20, [0; 6]);
        let input = DRAM_LOW + 0x100;
        let mut block = [0u8; 64];
        block[..4].copy_from_slice(&[b'a', b'b', b'c', 0x80]);
        block[63] = 24;
        for (i, byte) in block.into_iter().enumerate() { bus.write8(input + i as u32, byte).unwrap(); }
        for (desc, len, buf, next, eof) in [(DRAM_LOW, 16, input, DRAM_LOW + 16, 0), (DRAM_LOW + 16, 48, input + 16, 0, 1)] {
            bus.write32(desc, (1 << 31) | (eof << 30) | (len << 12) | len).unwrap();
            bus.write32(desc + 4, buf).unwrap();
            bus.write32(desc + 8, next).unwrap();
        }
        bus.write32(0x6003f100, 7).unwrap();
        bus.write32(0x6003f0e0, (1 << 21) | (DRAM_LOW & 0xfffff)).unwrap();
        bus.write32(0x6003b000, 2).unwrap();
        bus.write32(0x6003b00c, 1).unwrap();
        bus.write32(0x6003b01c, 1).unwrap();
        bus
    }

    #[test]
    fn hashes_sha256_across_dma_descriptors_and_returns_ownership() {
        let mut bus = fixture();
        bus.tick(1);
        let digest: Vec<_> = (0..8).flat_map(|n| bus.read32(0x6003b040 + n * 4).unwrap().to_le_bytes()).collect();
        assert_eq!(digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(bus.read32(0x6003b018).unwrap(), 0);
        assert_eq!(bus.read32(DRAM_LOW).unwrap() >> 31, 0);
        assert_eq!(bus.read32(DRAM_LOW + 16).unwrap() >> 31, 0);
        assert_eq!(bus.periph.gdma.gdma.out[0].eof_desc, DRAM_LOW + 16);
        assert_eq!(bus.periph.gdma.gdma.out[0].int_raw & 3, 3);
    }

    #[test]
    fn dma_continue_preserves_the_previous_hash_state() {
        let mut bus = fixture();
        let input = DRAM_LOW + 0x100;
        let mut padded = [0u8; 128];
        padded[..80].fill(b'a'); padded[80] = 0x80;
        padded[120..].copy_from_slice(&640u64.to_be_bytes());
        for (n, block) in padded.chunks_exact(64).enumerate() {
            for (i, byte) in block.iter().enumerate() { bus.write8(input + i as u32, *byte).unwrap(); }
            bus.write32(DRAM_LOW, (3 << 30) | (64 << 12) | 64).unwrap();
            bus.write32(DRAM_LOW + 8, 0).unwrap();
            bus.write32(0x6003f0e0, (1 << 21) | (DRAM_LOW & 0xfffff)).unwrap();
            bus.write32(if n == 0 { 0x6003b01c } else { 0x6003b020 }, 1).unwrap();
            bus.tick(1);
            assert!(!bus.periph.sha.busy);
        }
        let digest: Vec<_> = (0..8).flat_map(|n| bus.read32(0x6003b040 + n * 4).unwrap().to_le_bytes()).collect();
        assert_eq!(digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), "0f45e858fbc4176cdf4e411f88281edefc390ae5afe7df0f44cd9297f0a64580");
        assert_eq!(bus.periph.sha.blocks, 2);
    }

    #[test]
    fn cyclic_or_unowned_dma_does_not_hash_fabricated_bytes() {
        for unowned in [false, true] {
            let mut bus = fixture();
            if unowned { let word = bus.read32(DRAM_LOW + 16).unwrap(); bus.write32(DRAM_LOW + 16, word & !(1 << 31)).unwrap(); }
            else { bus.write32(DRAM_LOW + 8, DRAM_LOW).unwrap(); }
            bus.tick(1);
            assert_eq!(bus.periph.sha.blocks, 0);
            assert_eq!(bus.read32(0x6003b018).unwrap(), 1);
            assert_eq!(bus.read32(DRAM_LOW).unwrap() >> 31, 1);
            assert_eq!(bus.periph.gdma.gdma.out[0].int_raw & 3, 0);
        }
    }
}

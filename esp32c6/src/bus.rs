//! ESP32-C6 memory map.
//!
//! One address space for instructions and data: the 320 KB mask ROM at `0x4000_0000`, 512 KB of
//! HP SRAM at `0x4080_0000`, 16 KB of LP SRAM at `0x5000_0000`, and a single 16 MB flash cache
//! window at `0x4200_0000` behind a 256-entry MMU. The MMU is programmed through two SPI0
//! registers (item index / item content), not a memory-mapped table as on the C3.

use crate::periph::{Peripherals, CPU_SUB_BASE, CPU_SUB_END, PERIPH_BASE, PERIPH_END};
use riscv_rv32::bus::{Bus, Fault};

/// GPIO matrix output signal of RMT TX channel 0 (soc/gpio_sig_map.h); channel n is this + n.
pub const RMT_SIG_OUT0: u32 = 71;
pub const ROM_LOW: u32 = 0x4000_0000;
pub const ROM_HIGH: u32 = 0x4005_0000;
pub const SRAM_LOW: u32 = 0x4080_0000;
pub const SRAM_HIGH: u32 = 0x4088_0000;
pub const LP_SRAM_LOW: u32 = 0x5000_0000;
pub const LP_SRAM_HIGH: u32 = 0x5000_4000;
pub const FLASH_LOW: u32 = 0x4200_0000;
pub const FLASH_HIGH: u32 = 0x4300_0000;
pub const MMU_ENTRIES: usize = 256;
/// bit 9 marks an entry valid; bits 8:0 are the flash page
pub const MMU_VALID: u32 = 1 << 9;
/// SPI0 registers the MMU is driven through
pub const SPI_MMU_ITEM_CONTENT: u32 = 0x37c;
pub const SPI_MMU_ITEM_INDEX: u32 = 0x380;
pub const SPI_MMU_POWER_CTRL: u32 = 0x384;

pub struct SocBus {
    pub rom: Vec<u8>,
    pub sram: Vec<u8>,
    pub lp_sram: Vec<u8>,
    pub flash: Vec<u8>,
    pub mmu: [u32; MMU_ENTRIES],
    pub mmu_index: u32,
    /// SPI_MEM_MMU_POWER_CTRL: bits 4:3 select the page size (0 = 64 KB, 1 = 32, 2 = 16, 3 = 8)
    pub mmu_power_ctrl: u32,
    pub periph: Peripherals,
    /// a bare module: nothing on the pins
    pub board: esp_soc::Board,
    pub cycles: u64,
    gpio_cycle: u64,
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
            rom: vec![0; (ROM_HIGH - ROM_LOW) as usize],
            sram: vec![0; (SRAM_HIGH - SRAM_LOW) as usize],
            lp_sram: vec![0; (LP_SRAM_HIGH - LP_SRAM_LOW) as usize],
            flash: vec![0xff; flash_size],
            mmu: [0; MMU_ENTRIES], mmu_index: 0, mmu_power_ctrl: 0,
            periph: Peripherals::new(mac), board: Box::new(esp_soc::NoBoard),
            cycles: 0, gpio_cycle: 0, last_fault: None, irq_dirty: true, gpio_events: None, debug: Default::default(),
        }
    }

    /// log2 of the MMU page size
    #[inline]
    pub fn page_shift(&self) -> u32 { 16 - ((self.mmu_power_ctrl >> 3) & 3) }

    /// Resolve to (buffer, offset, writable). The flash window goes through the MMU.
    fn resolve(&mut self, addr: u32) -> Option<(&mut Vec<u8>, usize, bool)> {
        match addr {
            SRAM_LOW..=0x4087_FFFF => Some((&mut self.sram, (addr - SRAM_LOW) as usize, true)),
            ROM_LOW..=0x4004_FFFF => Some((&mut self.rom, (addr - ROM_LOW) as usize, false)),
            LP_SRAM_LOW..=0x5000_3FFF => Some((&mut self.lp_sram, (addr - LP_SRAM_LOW) as usize, true)),
            FLASH_LOW..=0x42FF_FFFF => {
                let shift = self.page_shift();
                let idx = ((addr - FLASH_LOW) >> shift) as usize;
                if idx >= MMU_ENTRIES { return None; }
                let entry = self.mmu[idx];
                if entry & MMU_VALID == 0 { return None; }
                let off = (((entry & 0x1ff) as usize) << shift) + (addr & ((1 << shift) - 1)) as usize;
                if off < self.flash.len() { Some((&mut self.flash, off, false)) } else { None }
            }
            _ => None,
        }
    }

    #[inline]
    fn is_periph(addr: u32) -> bool { (PERIPH_BASE..PERIPH_END).contains(&addr) || (CPU_SUB_BASE..CPU_SUB_END).contains(&addr) }

    fn periph_read(&mut self, addr: u32, size: u32) -> u32 {
        let w = if (CPU_SUB_BASE..CPU_SUB_END).contains(&addr) {
            self.periph.cpu_sub_read(addr - CPU_SUB_BASE)
        } else if (addr & !0xfff) == PERIPH_BASE + 0x2000 && matches!(addr & 0xfff, SPI_MMU_ITEM_CONTENT | SPI_MMU_ITEM_INDEX | SPI_MMU_POWER_CTRL) {
            match addr & 0xfff {
                SPI_MMU_ITEM_CONTENT => self.mmu[(self.mmu_index as usize) & (MMU_ENTRIES - 1)],
                SPI_MMU_ITEM_INDEX => self.mmu_index,
                _ => self.mmu_power_ctrl,
            }
        } else {
            self.periph.read32(addr & !3)
        };
        match size { 1 => (w >> ((addr & 3) * 8)) & 0xff, 2 => (w >> ((addr & 2) * 8)) & 0xffff, _ => w }
    }

    fn periph_write(&mut self, addr: u32, v: u32, size: u32) {
        let a = addr & !3;
        let merge = |old: u32| match size {
            4 => v,
            1 => { let sh = (addr & 3) * 8; (old & !(0xff << sh)) | ((v & 0xff) << sh) }
            _ => { let sh = (addr & 2) * 8; (old & !(0xffff << sh)) | ((v & 0xffff) << sh) }
        };
        if (CPU_SUB_BASE..CPU_SUB_END).contains(&a) {
            let old = self.periph.cpu_sub_read(a - CPU_SUB_BASE);
            self.periph.cpu_sub_write(a - CPU_SUB_BASE, merge(old));
            self.irq_dirty = true;
            return;
        }
        if (a & !0xfff) == PERIPH_BASE + 0x2000 {
            match a & 0xfff {
                SPI_MMU_ITEM_CONTENT => { self.mmu[(self.mmu_index as usize) & (MMU_ENTRIES - 1)] = merge(self.mmu[(self.mmu_index as usize) & (MMU_ENTRIES - 1)]) & 0x7ff; return; }
                SPI_MMU_ITEM_INDEX => { self.mmu_index = merge(self.mmu_index) & 0xff; return; }
                SPI_MMU_POWER_CTRL => { self.mmu_power_ctrl = merge(self.mmu_power_ctrl); return; }
                _ => {}
            }
        }
        let v = if size == 4 { v } else { merge(self.periph.read32(a)) };
        let old_drive=(self.periph.gpio.enable,self.periph.gpio.out);
        self.periph.write32(a, v);
        if (PERIPH_BASE+0x90000..PERIPH_BASE+0x92000).contains(&a) { self.board.gpio_waveform(self.gpio_cycle.max(self.cycles),&self.periph.gpio,128); }
        if old_drive!=(self.periph.gpio.enable,self.periph.gpio.out) {
            self.board.gpio_drive(self.cycles,self.periph.gpio.enable,self.periph.gpio.out);
            self.sync_board_inputs();
        }
        // A SPI flash command must complete before the guest reads its result (see the C3 notes:
        // running it at the quantum boundary loses the race and reads back zeros).
        if self.periph.spi_exec { self.run_spi(); }
        // TX_START: the radio's DMA reads the frame now, at the instruction that started it
        if self.periph.radio.tx_request.is_some() { self.radio_tx_fetch(); }
        // A GP-SPI transfer reaches the board now, after the GPIO edges that preceded it: the
        // display's D/C line is a GPIO the driver sets right before each transaction.
        if self.periph.spi2.dma_tx_pending.is_some() { self.spi2_dma_tx(); }
        if self.periph.spi2.has_pending_transfer() || !self.periph.gpio.changes.is_empty() { self.deliver_board_events(); }
        self.irq_dirty = true;
    }

    /// A word of SRAM for the DMA engines (descriptors and buffers live there).


    /// The 802.15.4 TX DMA: `buf[0]` is the PSDU length including the 2-byte FCS the hardware
    /// appends, `buf[1..]` the MAC frame. A buffer outside SRAM or a length under 2 is a driver
    /// bug on real silicon too; here it is reported and the transmission carries no bytes.
    fn radio_tx_fetch(&mut self) {
        let Some(addr) = self.periph.radio.tx_request.take() else { return };
        let o = addr.wrapping_sub(SRAM_LOW) as usize;
        let psdu = match self.sram.get(o) {
            Some(&len) => {
                let mac = (len & 0x7f).saturating_sub(2) as usize;
                if o + 1 + mac <= self.sram.len() { self.sram[o + 1..o + 1 + mac].to_vec() } else { Vec::new() }
            }
            None => Vec::new(),
        };
        if psdu.is_empty() { eprintln!("[802.15.4] TX_START with an empty or unmapped frame at {:#010x}", addr); }
        self.periph.radio.tx_loaded(psdu);
    }

    /// The 802.15.4 RX DMA: the completed frame, RSSI and LQI go where `DMA_RX_ADDR` points.
    fn radio_rx_store(&mut self) {
        let Some((addr, buf)) = self.periph.radio.rx_write.take() else { return };
        let o = addr.wrapping_sub(SRAM_LOW) as usize;
        if o + buf.len() <= self.sram.len() { self.sram[o..o + buf.len()].copy_from_slice(&buf); }
        else { eprintln!("[802.15.4] RX_DONE: DMA_RX_ADDR {:#010x} is not in SRAM, frame lost", addr); }
    }

    /// A frame from the medium (MAC header + payload, no FCS): started `Some(cycles)` ago (RX_DONE
    /// when its air time is up) or complete now (`None`; the buffer is written before the next
    /// instruction). Returns whether the radio took it; the interrupt lines are re-derived before
    /// the next instruction either way.
    pub fn radio_receive(&mut self, frame: &[u8], rssi: i8, lqi: u8, started_ago: Option<u64>) -> bool {
        let taken = self.periph.radio.receive(frame, rssi, lqi, started_ago);
        if self.periph.radio.rx_write.is_some() { self.radio_rx_store(); }
        self.irq_dirty = true;
        taken
    }

    /// GP-SPI2's MOSI data phase through the GDMA out-channel bound to it (trigger 0): walk the
    /// descriptor chain, hand the bytes to the SPI model, and complete the channel.
    fn spi2_dma_tx(&mut self) {
        let Some(bits) = self.periph.spi2.dma_tx_pending else { return };
        let Some(ch) = self.periph.gdma.gdma.out_channel_for(0) else { return };
        match esp_periph::spi_dma::transmit(&mut self.sram, SRAM_LOW, &mut self.periph.gdma.gdma.out[ch], bits) {
            Ok(data) => self.periph.spi2.complete_dma_tx(&data),
            Err(_) => self.periph.spi2.fail_dma_tx(),
        }

        self.irq_dirty = true;
    }

    /// Pin-level events to the board, in order: GPIO edges first, then what went out on the
    /// SPI, then completed RMT frames.
    fn deliver_board_events(&mut self) {
        if !self.periph.gpio.changes.is_empty() {
            let ch = std::mem::take(&mut self.periph.gpio.changes);
            if let Some(ev) = &mut self.gpio_events { for &(pin, level) in &ch { ev.push((self.cycles, pin, level)); } }
            self.board.gpio_changes(&ch);
        }
        if let Some(transfer) = self.periph.spi2.take_transfer() {
            let pins = esp_soc::spi::SpiLayout::C6.pins(&self.periph.gpio, &self.periph.spi2);
            let rx = self.board.spi_transfer_pins(2, pins, &transfer.tx, transfer.rx_len);
            self.periph.spi2.finish_transfer(transfer, &rx);
            self.irq_dirty = true;
        }
        if !self.periph.rmt.rmt.done.is_empty() { for (ch, bits) in std::mem::take(&mut self.periph.rmt.rmt.done) { for pin in self.periph.gpio.pins_for_signal(RMT_SIG_OUT0 + ch as u32) { self.board.rmt_frame(pin, &bits); } } self.irq_dirty = true; }
    }

    /// Execute a pending SPI1 command against the flash image.
    fn run_spi(&mut self) {
        self.periph.spi_exec = false;
        let mut no_psram = Vec::new();
        self.periph.spi1.0.execute(&mut self.flash, &mut no_psram);
        self.periph.spi1.0.dirty.clear();
    }

    /// Write straight into flash (image loaders, not the guest).
    pub fn write_flash(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
        if offset + data.len() > self.flash.len() { return Err("flash image too large".into()); }
        self.flash[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }

    pub fn load_bytes(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        // The mask ROM ELF links its .eh_frame at the flash window; nothing ever reads it.
        if (FLASH_LOW..FLASH_HIGH).contains(&addr) { return Ok(()); }
        for (i, b) in data.iter().enumerate() {
            let a = addr.wrapping_add(i as u32);
            match self.resolve(a) {
                Some((buf, off, _)) if off < buf.len() => buf[off] = *b,
                _ => return Err(format!("load: address {:#010x} not mapped", a)),
            }
        }
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
        let pending = std::mem::take(&mut self.periph.wifi.mac.tx_pending);
        for (slot, desc) in pending {
            let dw0 = self.read32(desc).unwrap_or(0); let pkt = self.read32(desc + 4).unwrap_or(0);
            let len = ((dw0 >> 14) & 0x3fff) as usize;
            let mut frame = Vec::with_capacity(len);
            for i in 0..len { frame.push(self.read8(pkt + i as u32).unwrap_or(0)); }
            // MAC v2 prefixes each MPDU with its 8-byte subframe descriptor.
            if frame.len() < 8 { continue; }
            let mpdu_len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize & 0x3fff;
            if mpdu_len > frame.len() - 8 { continue; }
            let frame = frame[8..8 + mpdu_len].to_vec();
            if self.periph.wifi.mac.log || self.debug.has("wifi-frames") { eprintln!("[wifi] TX slot {} desc {:#010x} pkt {:#010x} {}", slot, desc, pkt, crate::wifi::describe(&frame)); }
            self.periph.wifi.mac.tx_done(slot);
            self.irq_dirty = true;
            let now_us = self.cycles / (crate::periph::CPU_HZ / 1_000_000);
            if let Some(ap) = &mut self.periph.wifi.mac.ap {
                if let Some(data) = ap.on_station_tx(&frame, now_us) {
                    if let Some(eth) = crate::wifi::data_to_eth(&data) {
                        if eth.len() <= 1518 && self.periph.wifi.mac.eth_tx.len() < 64 { self.periph.wifi.mac.eth_tx.push(eth); }
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
        if now_us.wrapping_sub(self.periph.wifi.mac.last_rx_us) < 400 { return; }
        // ... but if software stops recycling altogether, don't stall the air forever: after 50 ms
        // the frame is dropped, exactly as a real ring would overflow.
        let busy = { let d = self.periph.wifi.mac.last_rx_desc; d != 0 && self.read32(d).unwrap_or(0) & (1 << 30) != 0 };
        if busy && now_us.wrapping_sub(self.periph.wifi.mac.last_rx_us) < 50_000 { return; }
        let mut due = { let ap = self.periph.wifi.mac.ap.as_mut().unwrap(); ap.step(now_us) };
        let eth_in = std::mem::take(&mut self.periph.wifi.mac.eth_rx);
        for e in eth_in { if let Some(f) = self.periph.wifi.mac.ap.as_mut().unwrap().data_from_ds(&e) { due.push(crate::wifi::AirFrame { at_us: now_us, frame: f }); } }
        if due.is_empty() { return; }
        // management responses (auth, assoc, probe) go before beacons: a connect exchange must not be
        // crowded out by beacon traffic
        due.sort_by_key(|a| (crate::wifi::is_beacon(&a.frame), a.at_us));
        let first = due.remove(0);
        self.wifi_rx_deliver(&first.frame, now_us);
        self.periph.wifi.mac.last_rx_us = now_us;
        if let Some(ap) = &mut self.periph.wifi.mac.ap { for a in due { ap.queue.push(a); } }
    }

    /// Write one received frame into the next RX descriptor (rx_ctrl header + frame + FCS) and raise the RX event.
    #[allow(clippy::identity_op, reason = "rx_state zero remains visible in the packed descriptor layout")]
    fn wifi_rx_deliver(&mut self, frame: &[u8], now_us: u64) {
        if self.periph.wifi.mac.rx_next == 0 { self.periph.wifi.mac.rx_dropped += 1; return; }
        let desc = self.periph.wifi.mac.rx_next | 0x4080_0000;
        let dw0 = self.read32(desc).unwrap_or(0); let buf = self.read32(desc + 4).unwrap_or(0); let next = self.read32(desc + 8).unwrap_or(0);
        let size = (dw0 & 0x3fff) as usize;
        let total = 92 + frame.len() + 4;
        if dw0 & (3 << 30) != 1 << 31 || buf == 0 || size < total { self.periph.wifi.mac.rx_dropped += 1; return; }
        let (chan, log) = { let ap = self.periph.wifi.mac.ap.as_ref().unwrap(); (ap.cfg.channel as u32, ap.log) };
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
        let mut header = [0u32; 23];
        header[0] = w0;
        header[2] = if bcast { 1 << 31 } else { 0 };
        header[3] = now_us as u32;
        header[5] = 0xa6 | (chan << 8);
        header[21] = ((frame.len()+4) as u32 & 0x3fff) | ((frame.len() as u32 & 0x3fff) << 16);
        for w in header { b.extend_from_slice(&w.to_le_bytes()); }
        b.extend_from_slice(frame); b.extend_from_slice(&crate::wifi::fcs(frame).to_le_bytes());
        let mut i = 0usize;
        while i + 4 <= b.len() { let v = u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]); let _ = self.write32(buf + i as u32, v); i += 4; }
        while i < b.len() { let _ = self.write8(buf + i as u32, b[i]); i += 1; }
        let ndw0 = (dw0 & !(0x3fff << 14)) | ((total as u32) << 14) | (1 << 30) | (1 << 31);
        let _ = self.write32(desc, ndw0);
        let w = &mut self.periph.wifi.mac;
        w.rx_last = desc & 0xf_ffff; w.rx_next = next & 0xf_ffff; w.last_rx_desc = desc; w.rx_frames += 1; w.events |= 1 << 14;
        if log { let d = crate::wifi::describe(frame); if d.contains("auth")||d.contains("assoc") { eprintln!("[wifi] RX AUTH/ASSOC -> desc {:#010x} buf {:#010x} {}", desc, buf, d); } else { eprintln!("[wifi] RX -> desc {:#010x} {}", desc, d); } }
        self.irq_dirty = true;
    }


    fn dma_parlio_step(&mut self) {
        let Some(index) = self.periph.gdma.gdma.out_channel_for(9) else { return; };
        let mut channel = self.periph.gdma.gdma.out[index];
        for _ in 0..80 {
            if !channel.running || self.periph.parlio.fifo.len() >= 64 { break; }
            let result = (|| -> Result<(), ()> {
                if channel.desc == 0 || channel.desc & 3 != 0 { return Err(()); }
                let word = self.read32(channel.desc).map_err(|_| ())?;
                let size = word & 0xfff;
                let length = (word >> 12) & 0xfff;
                let data = self.read32(channel.desc.checked_add(4).ok_or(())?).map_err(|_| ())?;
                let next = self.read32(channel.desc.checked_add(8).ok_or(())?).map_err(|_| ())?;
                if length > size || channel.buf_pos > length
                    || (channel.conf1 & (1 << 12) != 0 && word & (1 << 31) == 0) { return Err(()); }
                if channel.buf_pos == length {
                    if channel.conf0 & 4 != 0 { self.write32(channel.desc, word & !(1 << 31)).map_err(|_| ())?; }
                    channel.int_raw |= 1;
                    if word & (1 << 30) != 0 { channel.int_raw |= 2; channel.eof_desc = channel.desc; }
                    channel.desc = next; channel.buf_pos = 0;
                    if next == 0 { channel.running = false; channel.int_raw |= 8; }
                } else {
                    let byte = self.read8(data.checked_add(channel.buf_pos).ok_or(())?).map_err(|_| ())?;
                    self.periph.parlio.fifo.push_back(byte);
                    channel.buf_pos += 1;
                }
                Ok(())
            })();
            if result.is_err() { channel.running = false; channel.int_raw |= 4; break; }
        }
        self.irq_dirty |= channel.int_raw != self.periph.gdma.gdma.out[index].int_raw;
        self.periph.gdma.gdma.out[index] = channel;
    }

    /// Run the SPI1 controller if the guest just kicked it, advance device time, deliver what
    /// the devices produced to the board.
    fn devices(&mut self, cycles: u32) {
        if self.periph.spi_exec { self.run_spi(); }
        self.dma_parlio_step();
        self.periph.tick(cycles as u64);
        if let Some(frame) = self.periph.parlio.done.take() {
            let mut pins = Vec::new();
            for lane in 0..frame.width {
                for pin in self.periph.gpio.pins_for_signal(47 + u32::from(lane)) { pins.push((pin,lane)); }
            }
            self.board.parallel_output(&pins, &frame.samples, frame.clock_hz);
            self.irq_dirty = true;
        }
        self.sync_board_inputs();
        self.periph.i2s.rx_pcr_clock(self.periph.pcr.read(0x78), self.periph.pcr.read(0x7c));
        let bytes = self.periph.i2s.rx_data(cycles as u64, &self.periph.gpio, 15, 7, 0x1ff);
        if let Some(ch) = self.periph.gdma.gdma.in_channel_for(3) {
            let mut channel = self.periph.gdma.gdma.inp[ch];
            let eof = self.periph.i2s.rx_eof_bytes();
            esp_periph::i2s::receive_dma(self, &mut channel, &bytes, eof);
            self.irq_dirty |= channel.int_raw != self.periph.gdma.gdma.inp[ch].int_raw;
            self.periph.gdma.gdma.inp[ch] = channel;
        }

        if self.periph.radio.rx_write.is_some() { self.radio_rx_store(); }
        if self.periph.spi2.dma_tx_pending.is_some() { self.spi2_dma_tx(); }
        if self.periph.aes.dma_pending { self.aes_dma_step(); }
        if self.periph.sha.dma_pending { self.sha_dma_step(); }
        if !self.periph.wifi.mac.tx_pending.is_empty() { self.wifi_tx_step(); }
        if self.periph.wifi.mac.ap.is_some() {
            let now_us = self.cycles / (crate::periph::CPU_HZ / 1_000_000);
            if !self.periph.wifi.mac.relay && now_us.wrapping_sub(self.periph.wifi.mac.net_polled_us) >= 500 {
                self.periph.wifi.mac.net_polled_us = now_us;
                let tx = std::mem::take(&mut self.periph.wifi.mac.eth_tx);
                if let Some(net) = &mut self.periph.wifi.mac.net {
                    for e in tx { self.periph.wifi.mac.eth_rx.extend(net.handle(&e, now_us)); }
                    self.periph.wifi.mac.eth_rx.extend(net.poll(now_us));
                }
            }
            self.wifi_air_step();
        }
        self.deliver_board_events();
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
        self.gpio_cycle = self.cycles;
        self.devices(cycles);
        1
    }
    #[inline(always)]
    fn note_instruction_cycles(&mut self, cycles:u32) { self.gpio_cycle += u64::from(cycles); }
    #[inline(always)]
    fn note_pc(&mut self, pc: u32) { self.periph.misc.cur_pc = pc; }
    /// a peripheral write may have moved a line: the core's run stops so the machine re-derives it
    #[inline(always)]
    fn block_break(&self) -> bool { self.irq_dirty }
}

#[cfg(test)]
mod wifi_dma_tests {
    use super::*;
    #[test]
    fn relay_queues_are_bounded_and_reboot_preserves_only_configuration() {
        use esp_soc::SocBus as _;
        let mut bus = SocBus::new(4096, [0;6]);
        assert!(!bus.network_enable(true));
        bus.periph.wifi.mac.ap = Some(crate::wifi::VirtualAp::new(crate::wifi::ApConfig {
            ssid: "fixture".into(), bssid:[2,0,0,0,0,1], channel:6, psk:None,
        }, false));
        assert!(bus.network_enable(true));
        assert!(!bus.network_receive(&[0;13]));
        assert!(!bus.network_receive(&[0;1519]));
        for _ in 0..64 { assert!(bus.network_receive(&[0;14])); }
        assert!(!bus.network_receive(&[0;14]));
        bus.reboot([0;6]);
        assert!(bus.periph.wifi.mac.relay);
        assert!(bus.periph.wifi.mac.eth_rx.is_empty());
        assert_eq!(bus.periph.wifi.mac.ap.as_ref().unwrap().cfg.ssid, "fixture");
    }
    #[test]
    fn receive_uses_mac_v2_lengths_and_preserves_occupied_buffer() {
        let mut bus = SocBus::new(4096, [0;6]);
        bus.periph.wifi.mac.ap = Some(crate::wifi::VirtualAp::new(crate::wifi::ApConfig {
            ssid: "fixture".into(), bssid:[2,0,0,0,0,1], channel:6, psk:None,
        }, false));
        let (desc, buf) = (0x40801000, 0x40801100);
        bus.write32(desc, 2048 | (1 << 31)).unwrap();
        bus.write32(desc+4, buf).unwrap();
        bus.write32(desc+8, desc).unwrap();
        bus.write32(0x600a4084, desc).unwrap();
        let frame = [0x80;101];
        bus.wifi_rx_deliver(&frame, 12345);
        let control = bus.read32(desc).unwrap();
        assert_eq!((control >> 14) & 0x3fff, 197);
        assert_eq!(control & 0x3fff, 2048);
        assert_eq!(control >> 30, 3);
        assert_eq!(bus.read32(buf+12).unwrap(), 12345);
        assert_eq!(bus.read32(buf+20).unwrap(), 0x6a6);
        assert_eq!(bus.read32(buf+84).unwrap(), 105 | (101 << 16));
        assert_eq!(bus.read32(buf+92).unwrap(), 0x80808080);
        assert_eq!(bus.read32(0x600a4c48).unwrap(), 1 << 14);
        bus.wifi_rx_deliver(&[0;101], 100000);
        assert_eq!(bus.read32(buf+92).unwrap(), 0x80808080);
        assert_eq!(bus.periph.wifi.mac.rx_dropped, 1);
    }
}

#[cfg(test)]
mod sha_dma_tests {
    use super::*;
    use emu_core::Bus;

    fn fixture() -> SocBus {
        let mut bus = SocBus::new(4 << 20, [0; 6]);
        let input = SRAM_LOW + 0x100;
        let mut block = [0u8; 64];
        block[..4].copy_from_slice(&[b'a', b'b', b'c', 0x80]);
        block[63] = 24;
        for (i, byte) in block.into_iter().enumerate() { bus.write8(input + i as u32, byte).unwrap(); }
        for (desc, len, buf, next, eof) in [(SRAM_LOW, 16, input, SRAM_LOW + 16, 0), (SRAM_LOW + 16, 48, input + 16, 0, 1)] {
            bus.write32(desc, (1 << 31) | (eof << 30) | (len << 12) | len).unwrap();
            bus.write32(desc + 4, buf).unwrap();
            bus.write32(desc + 8, next).unwrap();
        }
        bus.write32(0x60080100, 7).unwrap();
        bus.write32(0x600800e0, (1 << 21) | (SRAM_LOW & 0xfffff)).unwrap();
        bus.write32(0x60089000, 2).unwrap();
        bus.write32(0x6008900c, 1).unwrap();
        bus.write32(0x6008901c, 1).unwrap();
        bus
    }

    #[test]
    fn hashes_sha256_across_dma_descriptors_and_returns_ownership() {
        let mut bus = fixture();
        bus.tick(1);
        let digest: Vec<_> = (0..8).flat_map(|n| bus.read32(0x60089040 + n * 4).unwrap().to_le_bytes()).collect();
        assert_eq!(digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(bus.read32(0x60089018).unwrap(), 0);
        assert_eq!(bus.read32(SRAM_LOW).unwrap() >> 31, 0);
        assert_eq!(bus.read32(SRAM_LOW + 16).unwrap() >> 31, 0);
        assert_eq!(bus.periph.gdma.gdma.out[0].eof_desc, SRAM_LOW + 16);
        assert_eq!(bus.periph.gdma.gdma.out[0].int_raw & 3, 3);
    }

    #[test]
    fn dma_continue_preserves_the_previous_hash_state() {
        let mut bus = fixture();
        let input = SRAM_LOW + 0x100;
        let mut padded = [0u8; 128];
        padded[..80].fill(b'a'); padded[80] = 0x80;
        padded[120..].copy_from_slice(&640u64.to_be_bytes());
        for (n, block) in padded.chunks_exact(64).enumerate() {
            for (i, byte) in block.iter().enumerate() { bus.write8(input + i as u32, *byte).unwrap(); }
            bus.write32(SRAM_LOW, (3 << 30) | (64 << 12) | 64).unwrap();
            bus.write32(SRAM_LOW + 8, 0).unwrap();
            bus.write32(0x600800e0, (1 << 21) | (SRAM_LOW & 0xfffff)).unwrap();
            bus.write32(if n == 0 { 0x6008901c } else { 0x60089020 }, 1).unwrap();
            bus.tick(1);
            assert!(!bus.periph.sha.busy);
        }
        let digest: Vec<_> = (0..8).flat_map(|n| bus.read32(0x60089040 + n * 4).unwrap().to_le_bytes()).collect();
        assert_eq!(digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), "0f45e858fbc4176cdf4e411f88281edefc390ae5afe7df0f44cd9297f0a64580");
        assert_eq!(bus.periph.sha.blocks, 2);
    }

    #[test]
    fn cyclic_or_unowned_dma_does_not_hash_fabricated_bytes() {
        for unowned in [false, true] {
            let mut bus = fixture();
            if unowned { let word = bus.read32(SRAM_LOW + 16).unwrap(); bus.write32(SRAM_LOW + 16, word & !(1 << 31)).unwrap(); }
            else { bus.write32(SRAM_LOW + 8, SRAM_LOW).unwrap(); }
            bus.tick(1);
            assert_eq!(bus.periph.sha.blocks, 0);
            assert_eq!(bus.read32(0x60089018).unwrap(), 1);
            assert_eq!(bus.read32(SRAM_LOW).unwrap() >> 31, 1);
            assert_eq!(bus.periph.gdma.gdma.out[0].int_raw & 3, 0);
        }
    }
}

#[cfg(test)]
mod parlio_dma_tests {
    use super::*;
    use esp_periph::Device;
    #[test]
    fn prefill_works_with_tx_clock_disabled_and_hands_back_descriptor() {
        let mut bus=SocBus::new(1024,[0;6]);
        let desc=SRAM_LOW+32; let data=SRAM_LOW+64;
        bus.write32(desc,(1<<31)|(1<<30)|(4<<12)|4).unwrap();
        bus.write32(desc+4,data).unwrap(); bus.write32(desc+8,0).unwrap();
        bus.write32(data,0x12345678).unwrap();
        let c=&mut bus.periph.gdma.gdma.out[0];
        c.running=true;c.desc=desc;c.peri_sel=9;c.conf0=4;c.conf1=1<<12;
        bus.periph.parlio.set_clock(0);
        bus.dma_parlio_step();
        assert_eq!(bus.periph.parlio.fifo.iter().copied().collect::<Vec<_>>(),vec![0x78,0x56,0x34,0x12]);
        assert_eq!(bus.periph.parlio.read(0x10),1<<31);
        assert_eq!(bus.periph.gdma.gdma.out[0].int_raw&15,11);
        assert!(!bus.periph.gdma.gdma.out[0].running);
        assert_eq!(bus.read32(desc).unwrap()>>31,0);
    }
    #[test]
    fn rejects_unowned_descriptor_without_output() {
        let mut bus=SocBus::new(1024,[0;6]); let desc=SRAM_LOW+32;
        bus.write32(desc,(4<<12)|4).unwrap();bus.write32(desc+4,SRAM_LOW+64).unwrap();bus.write32(desc+8,0).unwrap();
        let c=&mut bus.periph.gdma.gdma.out[0];c.running=true;c.desc=desc;c.peri_sel=9;c.conf1=1<<12;
        bus.dma_parlio_step();
        assert!(bus.periph.parlio.fifo.is_empty());
        assert_eq!(bus.periph.gdma.gdma.out[0].int_raw,4);
        assert!(!bus.periph.gdma.gdma.out[0].running);
    }
}

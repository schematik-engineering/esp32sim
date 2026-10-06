//! Opt-in C3 link controller, derived from the rev3 ROM and Arduino 3.3.11 / IDF 5.5.5.
//! All register and descriptor fields below are inferred, not public-header definitions or silicon validated.
//! See docs/evidence/ble-full-c3-2026-10-05/polls-disassembly.txt and the phase-1 through phase-4 receipts.
use std::collections::VecDeque;
use esp_soc::ble::peer::{Command, ReadStep, UuidRead};
use emu_core::ClockDomain;
use esp_periph::{Device, RegRam, WriteEffect};

const REQUEST: u32 = 1 << 31;
const TIMER: u32 = 1 << 11;
const HALF_US_CYCLES: u64 = crate::periph::CPU_HZ / 2_000_000;
const PERIOD: u64 = (1 << 28) * 625;

#[derive(Default)]
pub struct BleLc {
    ram: RegRam,
    state: Option<Box<State>>,
}

#[derive(Default)]
struct State {
    cycles: u64,
    reset: Option<u64>,
    latch: Option<u64>,
    alarm: Option<u64>,
    raw: u32,
    fifo: VecDeque<u32>,
    kicks: VecDeque<u8>,
    event: Option<Event>,
    scan_response: Vec<u8>,
    dropped: u64,
    pub_log: bool,
    scanning: bool,
    connecting: bool,
    read_request: Option<UuidRead>,
    stop_after_ms: Option<u32>,
    connection: Option<Connection>,
    packets: VecDeque<String>,
}

impl BleLc {
    pub fn enable(&mut self) { self.state = Some(Box::default()); }
    pub fn observe(&mut self, log: bool) { if let Some(s) = &mut self.state { s.pub_log = log; } }
    pub fn scan(&mut self, enabled: bool) { if let Some(s) = &mut self.state { s.scanning = enabled; } }
    pub fn connect(&mut self, stop_after_ms: Option<u32>) { if let Some(s) = &mut self.state { s.connecting = true; s.stop_after_ms = stop_after_ms; } }
    pub fn command(&mut self, command: &str) -> Result<(), String> {
        let command: Command = command.parse()?;
        let s = self.state.as_mut().ok_or("full BLE is disabled")?;
        match command {
            Command::Connect => s.connecting = true,
            Command::ReadUuid(service, characteristic) => {
                if s.read_request.is_some() || s.connection.as_ref().is_some_and(|c| c.gatt.is_some()) {
                    return Err("a BLE read is already pending".into());
                }
                s.read_request = Some(UuidRead::new(service, characteristic));
            }
            _ => return Err("full BLE supports connect and read-uuid SERVICE CHARACTERISTIC".into()),
        }
        Ok(())
    }
    pub fn scanning(&self) -> bool { self.state.as_ref().is_some_and(|s| s.scanning) }
    pub fn logging(&self) -> bool { self.state.as_ref().is_some_and(|s| s.pub_log) }
    pub fn take_observation(&mut self) -> Option<String> {
        let s = self.state.as_mut()?;
        if s.dropped != 0 { return Some(format!("[ble-observer] dropped={}", std::mem::take(&mut s.dropped))) }
        s.packets.pop_front()
    }
    pub fn enabled(&self) -> bool { self.state.is_some() }
}

fn half(bytes: &[u8], offset: usize) -> u16 { u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) }

struct Event {
    entry: usize,
    due: u64,
    advertising: Option<Advertising>,
    phase: ScanPhase,
    connection_phase: u8,
}

#[derive(Default)]
enum ScanPhase {
    #[default]
    Advertising,
    Request(u8),
    Receive(u8),
    Response(u8),
}

struct Advertising {
    activity: u16,
    channels: u8,
    pdu: Vec<u8>,
    scan_response: Vec<u8>,
}

impl State {
    fn raise(&mut self, source: u32) {
        self.raw |= source;
        // Coalesce repeated pending sources; TIMER and END are the modeled interrupt sources.
        if !self.fifo.contains(&source) { self.fifo.push_back(source); }
    }
    fn observe(&mut self, line: String) {
        if self.pub_log { eprintln!("{line}"); }
        if self.packets.len() == 1024 { self.packets.pop_front(); self.dropped += 1; }
        self.packets.push_back(line);
    }
}

impl BleLc {
    // Inferred mapping: r_emi_get_mem_addr_by_offset, 0x400069f6 / 0x40006a68.
    // The final eight mappings follow a seven-register gap (0x40006a8c).
    fn mapped(&self, offset: u32, len: usize, sram: &[u8]) -> Option<usize> {
        let mut mapping = None;
        let mut end = 0x10000;
        for i in 0..56 {
            let entry = self.ram.read(0x204 + (if i < 48 { i } else { i + 7 }) * 4);
            if entry == 0 { continue }
            let start = (entry >> 18) << 2;
            if start > offset { end = end.min(start); }
            if start <= offset && mapping.is_none_or(|(old, _)| old < start) {
                mapping = Some((start, entry));
            }
        }
        if offset.checked_add(len.try_into().ok()?)? > end { return None }
        let (start, entry) = mapping?;
        if entry & 0x3ffff == 0 { return None }
        let addr = (0x3fc00000 | ((entry << 2) & 0xffffc)).checked_add(offset - start)?;
        let index = addr.checked_sub(crate::bus::DRAM_LOW)? as usize + crate::bus::DRAM_IN_SRAM;
        sram.get(index..index.checked_add(len)?)?;
        Some(index)
    }

    fn advertising(&self, et: usize, sram: &[u8]) -> Result<Advertising, &'static str> {
        // Inferred CS halfword pointer: r_sch_prog_ble_push_hack, ET+8.
        let cs = self.mapped(half(sram, et + 8) as u32 * 2, 90, sram).ok_or("unmapped control structure")?;
        // Inferred CS fields: r_lld_adv_start 0x400186b8 (TX pointer),
        // 0x4001887e..ca (AdvA), 0x40018942 (channel map bits 7:5).
        // Legacy advertising CS format 4: r_lld_adv_start_set_cs 0x400181e4..e8.
        if half(sram, cs) & 0x1f != 4 { return Err("unsupported control structure format") }
        let channels = ((half(sram, cs + 38) >> 5) & 7) as u8;
        let first = half(sram, cs + 28) as u32;
        let mut next = first;
        let mut advertising = None;
        let mut scan_response = Vec::new();
        // Inferred nine 14-byte TX descriptors per activity: r_lld_adv_start 0x40018684..b8.
        for _ in 0..9 {
            let tx = self.mapped(next, 14, sram).ok_or("unmapped TX descriptor")?;
            // Inferred TX link/header/data: r_lld_adv_start_init_evt_param 0x40017b9e / 0x40017c50 (15-bit link),
            // r_lld_adv_adv_data_set 0x40015858/64 (length includes six-byte AdvA).
            let header = half(sram, tx + 2);
            let len = (header >> 8) as usize;
            if !(6..=37).contains(&len) { return Err("invalid legacy PDU length") }
            let data = self.mapped(half(sram, tx + 4) as u32, len - 6, sram).ok_or("unmapped AD buffer")?;
            let mut pdu = header.to_le_bytes().to_vec();
            pdu.extend_from_slice(&sram[cs + 6..cs + 12]);
            pdu.extend_from_slice(&sram[data..data + len - 6]);
            match header & 15 {
                0 | 2 | 6 if advertising.is_none() => advertising = Some(pdu),
                4 if scan_response.is_empty() => scan_response = pdu,
                _ => return Err("unsupported advertising descriptor chain"),
            }
            next = (half(sram, tx) & 0x7fff) as u32;
            if next == 0 || next == first {
                return Ok(Advertising { activity: half(sram, cs + 2) & 31, channels, pdu: advertising.ok_or("missing advertising PDU")?, scan_response });
            }
        }
        Err("unterminated TX descriptor chain")
    }

    pub(crate) fn service(&mut self, sram: &mut [u8]) {
        let Some(s) = &mut self.state else { return };
        let now = s.cycles;
        if s.event.is_none() {
            if let Some(index) = s.kicks.pop_front() {
                let Some(entry) = self.mapped(index as u32 * 16, 16, sram) else {
                    self.state.as_mut().unwrap().observe("[ble-error] unmapped event entry".into());
                    return;
                };
                // Inferred coarse/fine timestamp: r_sch_prog_push 0x40030f1a / 0x40030f3c / 0x40030f9e.
                let coarse = half(sram, entry + 2) as u64 | ((half(sram, entry + 4) as u64 & 0xfff) << 16);
                let target = coarse * 625 + 624u64.saturating_sub(half(sram, entry + 6) as u64);
                let delta = (target + PERIOD - now / HALF_US_CYCLES % PERIOD) % PERIOD;
                let due = if delta >= PERIOD / 2 { now } else { (now / HALF_US_CYCLES + delta) * HALF_US_CYCLES };
                self.state.as_mut().unwrap().event = Some(Event { entry, due, advertising: None, phase: ScanPhase::Advertising, connection_phase: 0 });
            }
        }
        let Some(mut event) = self.state.as_mut().unwrap().event.take() else { return };
        if event.due > now {
            self.state.as_mut().unwrap().event = Some(event);
            return;
        }
        let cs = self.mapped(half(sram, event.entry + 8) as u32 * 2, 90, sram);
        if let Some(cs) = cs {
            if half(sram, cs) & 31 == 3 {
                match self.connection_event(&mut event, cs, sram) {
                    Ok(true) => self.state.as_mut().unwrap().event = Some(event),
                    Ok(false) => {},
                    Err(reason) => {
                        self.state.as_mut().unwrap().observe(format!("[ble-error] {reason}"));
                        self.complete(event.entry, 4, sram);
                    }
                }
                return;
            }
        }
        if event.advertising.is_none() {
            if self.state.as_mut().unwrap().connection.take().is_some() {
                self.state.as_mut().unwrap().observe(format!("[ble-state] disconnected advertising_resumed hus={}", event.due / HALF_US_CYCLES));
            }
            match self.advertising(event.entry, sram) {
                Ok(advertising) => {
                    let s = self.state.as_mut().unwrap();
                    if advertising.scan_response != s.scan_response {
                        s.scan_response.clone_from(&advertising.scan_response);
                        if !s.scan_response.is_empty() {
                            s.observe(format!("[ble-config] SCAN_RSP {} data={}",
                                ad_fields(&s.scan_response[8..]), hex(&s.scan_response[8..])));
                        }
                    }
                    event.advertising = Some(advertising);
                }
                Err(reason) => {
                    self.state.as_mut().unwrap().observe(format!("[ble-error] {reason}"));
                    self.complete(event.entry, 4, sram);
                    return;
                }
            }
        }
        let advertising = event.advertising.as_mut().unwrap();
        match event.phase {
            ScanPhase::Request(channel) => {
                let pdu = if self.state.as_ref().unwrap().connecting { connect_request(&advertising.pdu) } else { scan_request(&advertising.pdu) };
                self.state.as_mut().unwrap().observe(format!("[ble-central] hus={} channel={channel} type={} pdu={}",
                    event.due / HALF_US_CYCLES, if pdu[0] & 15 == 5 { "CONNECT_IND" } else { "SCAN_REQ" }, hex(&pdu)));
                event.due += airtime(pdu.len());
                event.phase = ScanPhase::Receive(channel);
            }
            ScanPhase::Receive(channel) => {
                let pdu = if self.state.as_ref().unwrap().connecting { connect_request(&advertising.pdu) } else { scan_request(&advertising.pdu) };
                if let Err(reason) = self.receive(advertising.activity, channel, event.due - airtime(pdu.len()), &pdu, sram) {
                    self.state.as_mut().unwrap().observe(format!("[ble-error] {reason}"));
                    self.complete(event.entry, 4, sram);
                    return;
                }
                if pdu[0] & 15 == 5 {
                    let anchor = event.due + 17500 * HALF_US_CYCLES;
                    let s = self.state.as_mut().unwrap();
                    s.connection = Some(Connection { anchor, first_anchor: anchor, ..Connection::default() });
                    s.connecting = false;
                    self.complete(event.entry, 3, sram);
                    return;
                }
                event.due += 300 * HALF_US_CYCLES;
                event.phase = ScanPhase::Response(channel);
            }
            ScanPhase::Response(channel) => {
                self.emit(event.due, channel, &advertising.scan_response);
                event.due += airtime(advertising.scan_response.len()) + 300 * HALF_US_CYCLES;
                event.phase = ScanPhase::Advertising;
            }
            ScanPhase::Advertising if advertising.channels != 0 => {
                let channel = advertising.channels.trailing_zeros() as u8;
                advertising.channels &= !(1 << channel);
                self.emit(event.due, 37 + channel, &advertising.pdu);
                if (self.state.as_ref().unwrap().connecting && advertising.pdu[0] & 15 == 0)
                    || (self.scanning() && matches!(advertising.pdu[0] & 15, 0 | 6) && !advertising.scan_response.is_empty()) {
                    // Model choice: a virtual scanner responds exactly T_IFS after the PDU ends.
                    event.due += airtime(advertising.pdu.len()) + 300 * HALF_US_CYCLES;
                    event.phase = ScanPhase::Request(37 + channel);
                } else {
                    event.due += airtime(advertising.pdu.len()) + 600 * HALF_US_CYCLES;
                }
            }
            ScanPhase::Advertising => {
                self.complete(event.entry, 3, sram);
                return;
            }
        }
        self.state.as_mut().unwrap().event = Some(event);
    }

    fn emit(&mut self, cycles: u64, channel: u8, pdu: &[u8]) {
        let address = pdu[2..8].iter().rev().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
        let kind = match pdu[0] & 15 { 0 => "ADV_IND", 2 => "ADV_NONCONN_IND", 4 => "SCAN_RSP", _ => "ADV_SCAN_IND" };
        self.state.as_mut().unwrap().observe(format!("[ble-air] hus={} channel={channel} type={kind} AdvA={address} {} pdu={}",
            cycles / HALF_US_CYCLES, ad_fields(&pdu[8..]), hex(pdu)));
    }

    fn receive(&mut self, activity: u16, channel: u8, cycles: u64, pdu: &[u8], sram: &mut [u8]) -> Result<(), &'static str> {
        if pdu.len() < 2 || pdu.len() != pdu[1] as usize + 2 || channel > 39 {
            return Err("invalid received PDU");
        }
        // Inferred head/link: r_lld_core_init 0x4203cd80 / 0x4203ccb0.
        let logical = self.ram.read(0x24) & 0x7fff;
        let rx = self.mapped(logical, 20, sram).ok_or("unmapped RX descriptor")?;
        let link = half(sram, rx);
        if link & 0x8000 != 0 { return Err("RX descriptor still owned by guest") }
        // Inferred payload pointer: r_lld_adv_pkt_rx_send_scan_req_evt 0x4001644c..5e.
        let buffer = half(sram, rx + 18);
        if buffer == 0 { return Err("RX descriptor has no buffer") }
        let data = self.mapped(buffer as u32, pdu.len() - 2, sram).ok_or("unmapped RX buffer")?;
        sram[data..data + pdu.len() - 2].copy_from_slice(&pdu[2..]);
        // Inferred success/status/header/activity: r_lld_adv_pkt_rx 0x400165cc..d8,
        // r_lld_rxdesc_check 0x400203aa..ca. Only error-free host packets are supported.
        // Inferred RSSI/channel: r_lld_con_rx_channel_assess 0x40019dbc..dc8.
        // r_rf_rssi_convert 0x4002e026 sign-extends the low byte. Fixed -40 dBm is
        // a virtual-radio choice, not a signal-strength measurement.
        // Inferred timestamp: r_lld_con_rx_sync_time_update 0x4001a08e..ce / 0x4001a138..158.
        // Inferred 1M sync offset: r_lld_core_init 0x4203cdd6..ee writes
        // lld_exp_sync_pos_tab[0] = 40 + LC+0x90[14:8] microseconds.
        // r_lld_adv_pkt_rx_connect_post 0x40015e24..ca subtracts it and normalizes.
        let hus = cycles / HALF_US_CYCLES + 2 * (40 + ((self.ram.read(0x90) >> 8) & 127) as u64);
        let coarse = hus / 625;
        for (off, value) in [(2, 0), (4, half(pdu, 0)), (6, (channel as u16) << 8 | (-40i8 as u8 as u16)),
            (8, coarse as u16), (10, (coarse >> 16) as u16 & 0xfff),
            (12, activity << 11 | (624 - hus % 625) as u16), (14, 0)] {
            sram[rx + off..rx + off + 2].copy_from_slice(&value.to_le_bytes());
        }
        // RX+14 is inferred resolving-list pointer, zero for this public peer:
        // r_lld_adv_pkt_rx_send_scan_req_evt 0x40016470.
        // Publish ownership last; r_lld_rxdesc_free recycles buffers and clears this bit.
        sram[rx..rx + 2].copy_from_slice(&(link | 0x8000).to_le_bytes());
        self.ram.write(0x24, (link & 0x7fff) as u32);
        Ok(())
    }

    // The virtual central selects one fixed, valid CONNECT_IND parameter set.
    // Guest event entries determine receive windows; central anchors never follow
    // a late guest window. Missing windows must not silently shift the radio clock.
    fn connection_event(&mut self, event: &mut Event, cs: usize, sram: &mut [u8]) -> Result<bool, &'static str> {
        let mut c = self.state.as_mut().unwrap().connection.take().ok_or("connection event without central")?;
        let activity = half(sram, cs + 2) & 31;
        match event.connection_phase {
            0 => {
                if let Some(read) = self.state.as_mut().unwrap().read_request.take() {
                    if c.features {
                        c.att(&read.request());
                    } else {
                        // Virtual central: Bluetooth 5.0, no optional LL features.
                        c.outgoing.extend([vec![3,6,0x0c,9,0xff,0xff,1,0], vec![3,9,8,0,0,0,0,0,0,0,0]]);
                    }
                    c.gatt = Some(read);
                }
                // Inferred programmed unmapped channel: r_lld_con_evt_start_cbk 0x4001ae4e..aeaa.
                let control = half(sram, cs + 22);
                if control & (1 << 14) != 0 { return Err("CSA#2 not supported") }
                // Inferred channel map +34..38 and hop +22[12:8]: r_lld_con_start
                // 0x4001baf2..bb8c / 0x4001ba30..36. The guest programs the
                // previous unmapped channel; hardware adds one hop for this event.
                let map = sram[cs + 34..cs + 39].iter().enumerate().fold(0u64, |v, (i, b)| v | ((*b as u64) << (8 * i)));
                c.channel = csa1((control & 63) as u8, ((control >> 8) & 31) as u8, map)?;
                if c.channel != csa1(((c.events * 5) % 37) as u8, 5, (1 << 37) - 1)? {
                    return Err("central/guest channel mismatch");
                }
                let s = self.state.as_mut().unwrap();
                if c.events == 0 { s.observe("[ble-state] connected interval_us=30000 csa=1".into()); }
                s.observe(format!("[ble-connection] event={} window_hus={} anchor_hus={} channel={}",
                    c.events, event.due / HALF_US_CYCLES, c.anchor / HALF_US_CYCLES, c.channel));
                put_half(sram, event.entry, (half(sram, event.entry) & !0x38) | (2 << 3));
                // Inferred CS+26: r_lld_con_evt_start_cbk 0x4001aec0..af14 /
                // 0x4001b172..18c uses 2-us units, or 625-us units with bit 15.
                let window = half(sram, cs + 26);
                let width = (window & 0x7fff) as u64 * if window & 0x8000 == 0 { 4 } else { 1250 };
                if c.anchor < event.due || c.anchor > event.due + width * HALF_US_CYCLES {
                    return Err("central anchor outside receive window");
                }
                event.due = c.anchor;
                event.connection_phase = 1;
            }
            1 => {
                if self.state.as_ref().unwrap().stop_after_ms.is_some_and(|ms| c.anchor - c.first_anchor >= ms as u64 * crate::periph::CPU_HZ / 1000) {
                    if !c.stopped {
                        self.state.as_mut().unwrap().observe(format!("[ble-central] stopped hus={}", c.anchor / HALF_US_CYCLES));
                        c.stopped = true;
                    }
                    self.complete(event.entry, 3, sram);
                    c.anchor += 60_000 * HALF_US_CYCLES;
                    c.events += 1;
                    self.state.as_mut().unwrap().connection = Some(c);
                    return Ok(false);
                }
                let mut pdu = c.outgoing.front().cloned().unwrap_or_else(|| vec![1,0]);
                pdu[0] = c.central.header(pdu[0]);
                self.state.as_mut().unwrap().observe(format!("[ble-central] hus={} channel={} type=DATA pdu={}", event.due / HALF_US_CYCLES, c.channel, hex(&pdu)));
                c.central.sent = true;
                event.due += airtime(pdu.len());
                event.connection_phase = 2;
            }
            2 => {
                let mut pdu = c.outgoing.front().cloned().unwrap_or_else(|| vec![1,0]);
                pdu[0] = c.central.header(pdu[0]);
                // Inferred TX ownership/link/header: r_lld_con_tx 0x4001a410..16,
                // 0x4001a570..58e; r_lld_con_tx_prog 0x4001ac2e..66.
                if c.peripheral.acknowledge(pdu[0]) {
                    if let Some(packet) = c.pending.take() {
                        if let Some(tx) = packet.descriptor {
                            let next = half(sram, tx) & 0x7fff;
                            put_half(sram, tx, next | 0x8000);
                            put_half(sram, cs + 28, next);
                            // Inferred TX IRQ bit 1: r_rwble_isr_hack 0x40386ad2..ec.
                            // Live ip+0x6d4 -> 0x4000154c -> r_sch_prog_tx_isr.
                            // Bit 6 routes ip+0x6d0 to r_sch_prog_skip_isr instead.
                            self.state.as_mut().unwrap().raise(1 << 1);
                        }
                    }
                }
                if c.peripheral.accept(pdu[0]) {
                    self.receive(activity, c.channel, event.due - airtime(pdu.len()), &pdu, sram)?;
                    // r_rwble_isr_hack 0x40386aee..b08 -> r_sch_prog_rx_isr,
                    // then r_lld_con_rx_isr. ET must already be active (state 2).
                    self.state.as_mut().unwrap().raise(1 << 2);
                }
                event.due += 300 * HALF_US_CYCLES;
                event.connection_phase = 3;
            }
            3 => {
                if c.pending.is_none() {
                    let logical = half(sram, cs + 28) as u32;
                    let tx = self.mapped(logical, 14, sram).ok_or("unmapped connection TX")?;
                    let packet = if half(sram, tx) & 0x8000 == 0 {
                        let header = half(sram, tx + 2);
                        let len = (header >> 8) as usize;
                        let data = self.mapped(half(sram, tx + 4) as u32, len, sram).ok_or("unmapped connection payload")?;
                        let mut pdu = header.to_le_bytes().to_vec();
                        pdu.extend_from_slice(&sram[data..data + len]);
                        TxPacket { pdu, descriptor: Some(tx) }
                    } else {
                        TxPacket { pdu: vec![1, 0], descriptor: None }
                    };
                    c.pending = Some(packet);
                }
                // r_lld_con_tx 0x4001a410..16 only consumes descriptors with bit 15
                // set. Keep this descriptor and its payload until the peer ACKs it.
                let packet = c.pending.as_ref().unwrap();
                let mut pdu = packet.pdu.clone();
                pdu[0] = c.peripheral.header(pdu[0]);
                c.peripheral.sent = true;
                self.state.as_mut().unwrap().observe(format!("[ble-air] hus={} channel={} type=DATA pdu={}", event.due / HALF_US_CYCLES, c.channel, hex(&pdu)));
                if c.central.acknowledge(pdu[0]) { c.outgoing.pop_front(); }
                if c.central.accept(pdu[0]) {
                    match pdu[0] & 3 {
                        3 if pdu.len() > 2 => match pdu[2] {
                            0x0e | 8 => c.outgoing.push_back(vec![3,9,9,0,0,0,0,0,0,0,0]),
                            9 if !c.features => {
                                c.features = true;
                                if let Some(read) = &c.gatt { let request = read.request(); c.att(&request); }
                            }
                            9 | 0x0c => {},
                            _ => return Err("unsupported LL control procedure"),
                        },
                        2 => {
                            // ponytail: default ATT MTU 23 fits one 27-byte LL PDU.
                            // Add L2CAP reassembly before supporting larger negotiated MTUs.
                            if pdu.len() < 6 || half(&pdu, 2) as usize + 6 != pdu.len() || half(&pdu, 4) != 4 {
                                return Err("unsupported L2CAP packet");
                            }
                            let read = c.gatt.as_mut().ok_or("unsolicited ATT response")?;
                            match read.receive(&pdu[6..]) {
                                Ok(ReadStep::Request(request)) => c.att(&request),
                                Ok(ReadStep::Value(value)) => {
                                    self.state.as_mut().unwrap().observe(format!("[ble-att] value={} text={:?}", hex(&value), String::from_utf8_lossy(&value)));
                                    c.gatt = None;
                                }
                                Err(error) => {
                                    self.state.as_mut().unwrap().observe(format!("[ble-att] {error}"));
                                    c.gatt = None;
                                }
                            }
                        }
                        1 if pdu.len() == 2 => {},
                        _ => return Err("unsupported LL data fragment"),
                    }
                }
                event.due += airtime(pdu.len());
                if pdu[0] & 16 != 0 && event.due + 300 * HALF_US_CYCLES + airtime(2) < c.anchor + 60_000 * HALF_US_CYCLES {
                    event.due += 300 * HALF_US_CYCLES;
                    event.connection_phase = 1;
                } else {
                    event.connection_phase = 4;
                }
            }
            _ => {
                self.complete(event.entry, 3, sram);
                c.anchor += 60_000 * HALF_US_CYCLES;
                c.events += 1;
                self.state.as_mut().unwrap().connection = Some(c);
                return Ok(false);
            }
        }
        self.state.as_mut().unwrap().connection = Some(c);
        Ok(true)
    }

    fn complete(&mut self, entry: usize, status: u16, sram: &mut [u8]) {
        // Inferred successful ET state 3 / aborted state 4:
        // r_sch_prog_end_isr_handler 0x40387080..f0 supplies callback a2=(state==4).
        let value = (half(sram, entry) & !0x38) | (status << 3);
        sram[entry..entry + 2].copy_from_slice(&value.to_le_bytes());
        // Inferred END interrupt bit 5: r_rwble_isr_hack 0x40386b4a..68;
        // r_ip_funcs_p+0x6c0 resolves to r_sch_prog_end_isr_hack.
        self.state.as_mut().unwrap().raise(1 << 5);
    }
}

#[derive(Default)]
struct Connection {
    anchor: u64,
    first_anchor: u64,
    events: u32,
    channel: u8,
    central: Sequence,
    peripheral: Sequence,
    pending: Option<TxPacket>,
    stopped: bool,
    outgoing: VecDeque<Vec<u8>>,
    features: bool,
    gatt: Option<UuidRead>,
}
impl Connection {
    fn att(&mut self, pdu: &[u8]) {
        let mut packet = vec![2, (pdu.len() + 4) as u8, pdu.len() as u8, 0, 4, 0];
        packet.extend_from_slice(pdu);
        self.outgoing.push_back(packet);
    }
}
struct TxPacket {
    pdu: Vec<u8>,
    descriptor: Option<usize>,
}

#[derive(Default)]
struct Sequence { sn: u8, nesn: u8, sent: bool }
impl Sequence {
    fn header(&self, base: u8) -> u8 { (base & !12) | (self.sn << 3) | (self.nesn << 2) }
    fn acknowledge(&mut self, header: u8) -> bool {
        if self.sent && (header >> 2) & 1 != self.sn {
            self.sn ^= 1;
            self.sent = false;
            true
        } else { false }
    }
    fn accept(&mut self, header: u8) -> bool {
        if (header >> 3) & 1 == self.nesn {
            self.nesn ^= 1;
            true
        } else { false }
    }
}

// Bluetooth CSA#1: add hop, then remap through enabled channels in ascending order.
fn csa1(previous: u8, hop: u8, map: u64) -> Result<u8, &'static str> {
    if previous >= 37 || !(5..=16).contains(&hop) || map >> 37 != 0 || map.count_ones() < 2 {
        return Err("invalid CSA#1 parameters");
    }
    let unmapped = (previous + hop) % 37;
    if map & (1 << unmapped) != 0 { return Ok(unmapped) }
    let index = unmapped as u32 % map.count_ones();
    Ok((0..37).filter(|channel| map & (1 << channel) != 0).nth(index as usize).unwrap())
}

fn put_half(sram: &mut [u8], off: usize, value: u16) { sram[off..off + 2].copy_from_slice(&value.to_le_bytes()); }

fn connect_request(advertising: &[u8]) -> Vec<u8> {
    let mut pdu = vec![5 | ((advertising[0] & 0x40) << 1), 34, 1, 0, 0, 0, 0, 2];
    pdu.extend_from_slice(&advertising[2..8]);
    pdu.extend_from_slice(&[0x70, 0x83, 0x32, 0x9a, 0x56, 0x34, 0x12, 1, 6, 0,
        24, 0, 0, 0, 200, 0, 0xff, 0xff, 0xff, 0xff, 0x1f, 5]);
    pdu
}

fn airtime(len: usize) -> u64 { 8 * (len as u64 + 8) * 2 * HALF_US_CYCLES }

fn scan_request(advertising: &[u8]) -> Vec<u8> {
    // Public simulated scanner address 02:00:00:00:00:01; RxAdd follows advertiser TxAdd.
    let mut pdu = vec![3 | ((advertising[0] & 0x40) << 1), 12, 1, 0, 0, 0, 0, 2];
    pdu.extend_from_slice(&advertising[2..8]);
    pdu
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

fn ad_fields(data: &[u8]) -> String {
    let mut fields = esp_soc::ble::peer::advertising_fields(data);
    for (kind, value) in esp_soc::ble::peer::ad_structures(data) {
        match (kind, value) {
            (1, [flags]) => fields.push(format!("flags=0x{flags:02x}")),
            (0x12, [lo, hi, lo2, hi2]) => fields.push(format!("connection_interval_units={}:{}",
                u16::from_le_bytes([*lo, *hi]), u16::from_le_bytes([*lo2, *hi2]))),
            (2 | 3 | 6 | 7 | 8 | 9, _) => {},
            _ => fields.push(format!("ad_{kind:02x}={}", hex(value))),
        }
    }
    fields.join(" ")
}

impl Device for BleLc {
    fn read(&mut self, off: u32) -> u32 {
        let Some(s) = &self.state else { return self.ram.read(off) };
        match off {
            // Inferred identity required by app r_lld_core_init, load 0x4203ce76.
            0x004 => 0x0900_1b00,
            // Inferred masked status, read by ROM r_rwble_isr (0x4002e8ee).
            0x010 => s.raw & self.ram.read(0x00c),
            // Inferred W1C readback for read/modify/write acknowledgements in r_rwble_isr.
            0x018 => 0,
            // Inferred FIFO: r_rwble_isr_hack at 0x403869a6 extracts count [9:5]
            // and status [30:10], then writes bit 0 to pop. r_lld_core_init resets bit 31.
            0x2d8 => (self.ram.read(off) & 0x1e) | ((s.fifo.len() as u32) << 5) | (s.fifo.front().copied().unwrap_or(0) << 10),
            _ => self.ram.read(off),
        }
    }

    fn write(&mut self, off: u32, v: u32) -> WriteEffect {
        let Some(s) = &mut self.state else {
            self.ram.write(off, v);
            return WriteEffect::NONE;
        };
        match off {
            0x004 | 0x010 | 0x020 => {},
            // Inferred index/request: r_sch_prog_ble_push_hack, 0x40387638.
            0x100 => {
                self.ram.write(off, v);
                if v & REQUEST != 0 && s.kicks.len() < 16 { s.kicks.push_back((v & 15) as u8); }
            }
            // Inferred reset/disable completion: r_rwip_driver_init / r_rwble_hw_disable.
            // One half-microsecond latency is a model choice, not a measured hardware delay.
            0x000 => {
                self.ram.write(off, v);
                if v & REQUEST != 0 { s.reset = Some(s.cycles + HALF_US_CYCLES); }
            }
            // Inferred atomic snapshot: r_rwip_time_get polls bit 31, masks 28 bits,
            // and returns 624 minus the fine register. Reads never advance time.
            0x01c => {
                if v & REQUEST != 0 {
                    self.ram.write(off, self.ram.read(off) | REQUEST);
                    s.latch = Some(s.cycles + HALF_US_CYCLES);
                }
            }
            // Inferred W1C: r_rwip_timer_hus_set and r_rwble_isr.
            0x018 => s.raw &= !v,
            0x2d8 => {
                self.ram.write(off, v & 0x1e);
                if v & REQUEST != 0 { s.fifo.clear(); } else if v & 1 != 0 { s.fifo.pop_front(); }
            }
            // Inferred alarm pair: r_rwip_timer_hus_set writes coarse then 624-hus.
            // A target behind now by less than half the counter period is due immediately.
            0x0f0 => {
                self.ram.write(off, v & 0x3ff);
                let target = (self.ram.read(0x0ec) as u64 & 0x0fff_ffff) * 625
                    + 624u64.saturating_sub((v & 0x3ff) as u64);
                let now = s.cycles / HALF_US_CYCLES;
                let delta = (target + PERIOD - now % PERIOD) % PERIOD;
                s.alarm = Some(if delta >= PERIOD / 2 { s.cycles } else { (now + delta) * HALF_US_CYCLES });
            }
            _ => self.ram.write(off, v),
        }
        WriteEffect::NONE
    }

    fn clock(&self) -> Option<ClockDomain> { self.state.as_ref().map(|_| ClockDomain::Cpu) }
    fn has_deadline(&self) -> bool { self.enabled() }
    fn next_deadline(&self) -> Option<u64> {
        let s = self.state.as_ref()?;
        [s.reset, s.latch, s.alarm, s.event.as_ref().map(|e| e.due)].into_iter().flatten().min().map(|t| t.saturating_sub(s.cycles).max(1))
    }
    fn irq_sources(&self) -> u64 {
        self.state.as_ref().is_some_and(|s| s.raw & self.ram.read(0x00c) != 0) as u64
    }
    fn tick(&mut self, ticks: u64) {
        let Some(s) = &mut self.state else { return };
        s.cycles += ticks;
        if s.reset.is_some_and(|t| t <= s.cycles) {
            s.reset = None;
            s.alarm = None;
            s.event = None;
            s.kicks.clear();
            s.connection = None;
            s.raw = 0;
            s.fifo.clear();
            self.ram.write(0, self.ram.read(0) & !REQUEST);
        }
        if s.latch.is_some_and(|t| t <= s.cycles) {
            // Snapshot at completion, even if the scheduler advances beyond the deadline.
            let hus = s.latch.take().unwrap() / HALF_US_CYCLES;
            self.ram.write(0x01c, (hus / 625) as u32 & 0x0fff_ffff);
            self.ram.write(0x020, 624 - (hus % 625) as u32);
        }
        if s.alarm.is_some_and(|t| t <= s.cycles) {
            s.alarm = None;
            s.raise(TIMER);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertising_fixture() -> (BleLc, Vec<u8>) {
        let mut d = BleLc::default();
        d.enable();
        // One synthetic SRAM mapping; pointers exercise the actual exchange-memory decoder.
        d.write(0x204, 0x20000);
        let mut ram = vec![0; 0x14000];
        let base = crate::bus::DRAM_IN_SRAM;
        for (offset, value) in [(0, 2), (2, 10), (6, 624), (8, 0x200),
            (0x400, 4), (0x41c, 0x1400), (0x426, 0xe0),
            (0x1400, 0x140e), (0x1402, 0x0900), (0x1404, 0x2c00),
            (0x140e, 0x1400), (0x1410, 0x0a04), (0x1412, 0x2400)] {
            ram[base + offset..base + offset + 2].copy_from_slice(&(value as u16).to_le_bytes());
        }
        ram[base + 0x406..base + 0x40c].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        ram[base + 0x2c00..base + 0x2c03].copy_from_slice(&[2, 1, 6]);
        ram[base + 0x2400..base + 0x2404].copy_from_slice(&[3, 9, b'x', b'y']);
        (d, ram)
    }

    #[test]
    fn descriptor_chain_maps_address_data_and_conditional_scan_response() {
        let (mut d, mut ram) = advertising_fixture();
        let et = crate::bus::DRAM_IN_SRAM;
        let a = d.advertising(et, &ram).unwrap();
        assert_eq!(a.pdu, [0, 9, 6, 5, 4, 3, 2, 1, 2, 1, 6]);
        assert_eq!(&a.scan_response[8..], &[3, 9, b'x', b'y']);
        assert_eq!(a.channels, 7);
        ram[et + 0x1403] = 38;
        assert!(d.advertising(et, &ram).is_err());
        ram[et + 0x1403] = 9;
        // An unmapped region must not fall back to the preceding mapping.
        d.write(0x208, 0x2c000000);
        assert!(d.advertising(et, &ram).is_err());
        assert!(d.mapped(0x2bff, 2, &ram).is_none());
        assert!(d.mapped(u32::MAX, usize::MAX, &ram).is_none());
    }

    #[test]
    fn channels_run_in_time_then_completion_is_masked_and_acknowledged() {
        let (mut d, mut ram) = advertising_fixture();
        d.write(0x100, REQUEST);
        d.service(&mut ram);
        assert!(d.take_observation().is_none());
        d.tick(10 * 625 * HALF_US_CYCLES - 1);
        d.service(&mut ram);
        assert!(d.take_observation().is_none());
        d.tick(1);
        d.service(&mut ram);
        assert!(d.take_observation().unwrap().starts_with("[ble-config] SCAN_RSP name=\"xy\""));
        let first = d.take_observation().unwrap();
        assert!(first.contains("hus=6250 channel=37 type=ADV_IND AdvA=01:02:03:04:05:06"));
        for channel in [38, 39] {
            let delta = d.next_deadline().unwrap();
            d.tick(delta - 1);
            d.service(&mut ram);
            assert!(d.take_observation().is_none());
            d.tick(1);
            d.service(&mut ram);
            assert!(d.take_observation().unwrap().contains(&format!("channel={channel} type=ADV_IND")));
        }
        assert_eq!(d.irq_sources(), 0);
        d.tick(d.next_deadline().unwrap());
        d.service(&mut ram);
        assert_eq!((half(&ram, crate::bus::DRAM_IN_SRAM) >> 3) & 7, 3);
        assert_eq!(d.read(0x10), 0);
        d.write(0xc, 1 << 5);
        assert_eq!(d.read(0x10), 1 << 5);
        assert_eq!(d.irq_sources(), 1);
        assert_eq!(d.read(0x2d8), (1 << 5) | (1 << 15));
        d.write(0x18, 1 << 5);
        d.write(0x2d8, 1);
        assert_eq!(d.irq_sources(), 0);
        assert_eq!(d.read(0x2d8), 0);
        assert!(d.take_observation().is_none());
        assert_eq!(d.next_deadline(), None);
    }

    #[test]
    fn channel_map_reset_and_bad_descriptor_do_not_transmit_extra_packets() {
        for map in 0..8 {
            let (mut d, mut ram) = advertising_fixture();
            ram[crate::bus::DRAM_IN_SRAM + 0x426] = map << 5;
            d.write(0x100, REQUEST);
            d.service(&mut ram);
            for _ in 0..4 {
                if let Some(delta) = d.next_deadline() { d.tick(delta); d.service(&mut ram); }
            }
            let mut emitted = Vec::new();
            while let Some(line) = d.take_observation() {
                if line.starts_with("[ble-air]") { emitted.push(line); }
            }
            let channels: Vec<_> = (0..3).filter(|ch| map & (1 << ch) != 0).map(|ch| ch + 37).collect();
            assert_eq!(emitted.len(), channels.len());
            for (line, channel) in emitted.iter().zip(channels) { assert!(line.contains(&format!("channel={channel}"))); }
        }
        let (mut d, mut ram) = advertising_fixture();
        d.write(0x100, REQUEST);
        d.service(&mut ram);
        d.write(0, REQUEST);
        d.tick(1_000_000);
        d.service(&mut ram);
        assert!(d.take_observation().is_none());
        ram[crate::bus::DRAM_IN_SRAM + 0x1403] = 255;
        d.write(0x100, REQUEST);
        d.service(&mut ram);
        assert!(d.take_observation().unwrap().starts_with("[ble-error]"));
        assert_eq!((half(&ram, crate::bus::DRAM_IN_SRAM) >> 3) & 7, 4);
    }


    #[test]
    fn active_scan_uses_guest_rx_ring_and_ifs_before_response() {
        let (mut d, mut ram) = advertising_fixture();
        let base = crate::bus::DRAM_IN_SRAM;
        d.scan(true);
        d.write(0x24, 0x1000);
        for index in 0..3 {
            let rx = base + 0x1000 + index as usize * 20;
            ram[rx..rx + 2].copy_from_slice(&(0x1000 + ((index + 1) % 3) * 20u16).to_le_bytes());
            ram[rx + 18..rx + 20].copy_from_slice(&(0x7800 + index * 32u16).to_le_bytes());
        }
        d.write(0x100, REQUEST);
        d.service(&mut ram);
        let mut lines = Vec::new();
        while let Some(delta) = d.next_deadline() {
            d.tick(delta - 1);
            d.service(&mut ram);
            assert!(d.take_observation().is_none(), "radio action before deadline");
            d.tick(1);
            d.service(&mut ram);
            while let Some(line) = d.take_observation() {
                if !line.starts_with("[ble-config]") { lines.push(line); }
            }
        }
        assert_eq!(lines.len(), 9);
        let time = |line: &str| -> u64 { line.split_whitespace().find_map(|v| v.strip_prefix("hus=")).unwrap().parse().unwrap() };
        for (index, exchange) in lines.as_chunks::<3>().0.iter().enumerate() {
            assert!(exchange[0].contains("type=ADV_IND"));
            assert!(exchange[1].contains("type=SCAN_REQ"));
            assert!(exchange[2].contains("type=SCAN_RSP"));
            assert!(exchange[2].contains("name=\"xy\""));
            assert_eq!(time(&exchange[1]) - time(&exchange[0]), airtime(11) / HALF_US_CYCLES + 300);
            assert_eq!(time(&exchange[2]) - time(&exchange[1]), airtime(14) / HALF_US_CYCLES + 300);
            for line in exchange { assert!(line.contains(&format!("channel={}", 37 + index))); }
            let rx = base + 0x1000 + index * 20;
            assert_ne!(half(&ram, rx) & 0x8000, 0);
            assert_eq!(half(&ram, rx + 2), 0);
            assert_eq!(half(&ram, rx + 4), 0x0c03);
            assert_eq!(half(&ram, rx + 6), ((37 + index as u16) << 8) | 216);
            assert_eq!(&ram[base + 0x7800 + index * 32..base + 0x780c + index * 32],
                &[1, 0, 0, 0, 0, 2, 6, 5, 4, 3, 2, 1]);
        }
        assert_eq!(d.read(0x24), 0x1000);
        assert_eq!(d.state.as_ref().unwrap().raw, 1 << 5);
        let snapshot = ram.clone();
        assert_eq!(d.receive(0, 37, 0, &scan_request(&[0; 8]), &mut ram), Err("RX descriptor still owned by guest"));
        assert_eq!(snapshot, ram);
        assert!(d.receive(0, 0, 0, &[3, 12], &mut ram).is_err());
    }

    #[test]
    fn csa1_hops_and_remaps_sparse_channels() {
        let all = (1 << 37) - 1;
        assert_eq!((0..9).map(|n| csa1((n * 5) % 37, 5, all).unwrap()).collect::<Vec<_>>(),
            [5, 10, 15, 20, 25, 30, 35, 3, 8]);
        let map = (1 << 0) | (1 << 10) | (1 << 36);
        assert_eq!(csa1(0, 5, map), Ok(36)); // 5 % 3 -> third used channel
        assert_eq!(csa1(5, 5, map), Ok(10)); // already enabled
        assert_eq!(csa1(32, 5, map), Ok(0)); // modulo-37 wrap
        for (prev, hop, map) in [(37, 5, all), (0, 4, all), (0, 17, all), (0, 5, 1), (0, 5, 1 << 37)] {
            assert!(csa1(prev, hop, map).is_err());
        }
    }

    #[test]
    fn sequence_retransmits_until_ack_and_rejects_duplicate_payload() {
        let mut central = Sequence::default();
        let mut peripheral = Sequence::default();
        central.sent = true;
        let request = central.header(1);
        assert!(peripheral.accept(request));
        assert!(!peripheral.accept(request));
        peripheral.sent = true;
        let response = peripheral.header(0x13);
        assert_eq!(response, 0x17); // MD preserved, ACK next central SN=1.
        assert!(central.acknowledge(response));
        assert!(!central.acknowledge(response));
        assert!(central.accept(response));
        assert!(!central.accept(response));
        assert!(!peripheral.acknowledge(request)); // retransmit response unchanged
        assert_eq!(peripheral.header(0x13), response);
        assert!(peripheral.acknowledge(central.header(1)));
        assert_eq!(peripheral.sn, 1);
    }

    #[test]
    fn sync_timestamp_recovers_packet_start_across_half_slots_and_wrap() {
        for start in [0, 536, 537, 624, 625, PERIOD - 44, PERIOD + 625] {
            let (mut d, mut ram) = advertising_fixture();
            let base = crate::bus::DRAM_IN_SRAM;
            d.write(0x90, 4 << 8); // Guest lld_exp_sync_pos_tab[0] = 44 us.
            d.write(0x24, 0x1000);
            put_half(&mut ram, base + 0x1000, 0x1014);
            put_half(&mut ram, base + 0x1012, 0x7800);
            d.receive(1, 5, start * HALF_US_CYCLES, &[1, 0], &mut ram).unwrap();
            let rx = base + 0x1000;
            let coarse = half(&ram, rx + 8) as u64 | ((half(&ram, rx + 10) as u64) << 16);
            let fine = half(&ram, rx + 12) as u64 & 1023;
            // r_lld_adv_pkt_rx_connect_post subtracts 2*44, then borrows 625.
            let recovered = (coarse * 625 + 624 - fine + PERIOD - 88) % PERIOD;
            assert_eq!(recovered, start % PERIOD);
            assert_eq!(half(&ram, rx + 12) >> 11, 1);
        }
    }

    #[test]
    fn tx_descriptor_is_released_only_after_peer_ack() {
        let (mut d, mut ram) = advertising_fixture();
        let base = crate::bus::DRAM_IN_SRAM;
        d.write(0x24, 0x1000);
        put_half(&mut ram, base + 0x1000, 0x1014);
        put_half(&mut ram, base + 0x1012, 0x7800);
        d.state.as_mut().unwrap().connection = Some(Connection {
            channel: 5,
            peripheral: Sequence { sent: true, ..Sequence::default() },
            pending: Some(TxPacket { pdu: vec![3, 1, 0x0c], descriptor: Some(base + 0x1400) }),
            ..Connection::default()
        });
        let mut event = Event { entry: base, due: airtime(2), advertising: None,
            phase: ScanPhase::Advertising, connection_phase: 2 };
        d.connection_event(&mut event, base + 0x400, &mut ram).unwrap();
        assert_eq!(half(&ram, base + 0x1400) & 0x8000, 0);
        assert!(d.state.as_ref().unwrap().connection.as_ref().unwrap().pending.is_some());
        d.state.as_mut().unwrap().connection.as_mut().unwrap().central.nesn = 1;
        event.connection_phase = 2;
        d.connection_event(&mut event, base + 0x400, &mut ram).unwrap();
        assert_ne!(half(&ram, base + 0x1400) & 0x8000, 0);
        assert_eq!(half(&ram, base + 0x41c), 0x140e);
        assert!(d.state.as_ref().unwrap().connection.as_ref().unwrap().pending.is_none());
        assert_ne!(d.state.as_ref().unwrap().raw & (1 << 1), 0);
    }

    #[test]
    fn latch_is_atomic_and_wraps_in_modeled_time() {
        let mut d = BleLc::default();
        d.enable();
        for hus in [0, 623, 624, 625, PERIOD - 1, PERIOD, PERIOD + 625] {
            d.state.as_mut().unwrap().cycles = hus * HALF_US_CYCLES;
            d.write(0x01c, REQUEST);
            for _ in 0..10 { assert_ne!(d.read(0x01c) & REQUEST, 0); }
            d.tick(HALF_US_CYCLES - 1);
            assert_ne!(d.read(0x01c) & REQUEST, 0);
            d.tick(1);
            let snapshot = (d.read(0x01c), d.read(0x020));
            assert_eq!(snapshot, (((hus + 1) / 625) as u32 & 0x0fff_ffff, 624 - ((hus + 1) % 625) as u32));
            d.tick(1000);
            assert_eq!(snapshot, (d.read(0x01c), d.read(0x020)));
        }
    }

    #[test]
    fn reset_completes_without_polling_and_cancels_alarm() {
        let mut d = BleLc::default();
        d.enable();
        for control in [0x100, 0] {
            d.write(0x0ec, 10);
            d.write(0x0f0, 624);
            d.write(0, REQUEST | control);
            d.tick(HALF_US_CYCLES - 1);
            assert_eq!(d.read(0), REQUEST | control);
            d.tick(1);
            assert_eq!(d.read(0), control);
            assert_eq!(d.read(4), 0x0900_1b00);
            d.tick(1_000_000);
            assert_eq!(d.state.as_ref().unwrap().raw, 0);
        }
    }

    #[test]
    fn alarm_mask_ack_rearm_and_wrap() {
        let mut d = BleLc::default();
        d.enable();
        for now in [0, PERIOD - 2, PERIOD + 10] {
            d.state.as_mut().unwrap().cycles = now * HALF_US_CYCLES;
            let target = (now + 3) % PERIOD;
            d.write(0x00c, 0);
            d.write(0x0ec, (target / 625) as u32);
            d.write(0x0f0, 624 - (target % 625) as u32);
            assert_eq!(d.next_deadline(), Some(3 * HALF_US_CYCLES));
            d.tick(3 * HALF_US_CYCLES - 1);
            assert_eq!(d.state.as_ref().unwrap().raw, 0);
            d.tick(1);
            assert_eq!(d.state.as_ref().unwrap().raw, TIMER);
            assert_eq!(d.irq_sources(), 0);
            d.write(0x00c, TIMER);
            assert_eq!(d.read(0x010), TIMER);
            assert_eq!(d.irq_sources(), 1);
            assert_eq!(d.read(0x2d8), (TIMER << 10) | (1 << 5));
            d.write(0x2d8, 1);
            assert_eq!(d.read(0x2d8), 0);
            d.write(0x018, 1);
            assert_eq!(d.irq_sources(), 1);
            d.write(0x018, TIMER);
            assert_eq!(d.irq_sources(), 0);
            d.tick(10_000);
            assert_eq!(d.read(0x010), 0);
        }
    }

    #[test]
    fn optional_device_routes_source_eight_only_when_enabled() {
        let mut p = crate::periph::Peripherals::new([0; 6]);
        assert_eq!(p.ble_lc.clock(), None);
        assert!(!p.ble_lc.has_deadline());
        p.write32(0x6003101c, REQUEST);
        p.tick(1000);
        assert_eq!(p.read32(0x6003101c), REQUEST);
        assert!(p.misc.active_optional.is_empty());
        p.ble_lc.enable();
        p.write32(0x600c2020, 9);
        p.write32(0x6003100c, TIMER);
        p.write32(0x600310ec, 1);
        p.write32(0x600310f0, 624);
        p.tick(625 * HALF_US_CYCLES);
        p.refresh_lines();
        assert_eq!(p.source_status()[0] & (1 << 8), 1 << 8);
        assert_ne!(p.intc.lines.level & (1 << 9), 0);
        p.write32(0x60031018, TIMER);
        p.refresh_lines();
        assert_eq!(p.intc.lines.level & (1 << 9), 0);
    }
}

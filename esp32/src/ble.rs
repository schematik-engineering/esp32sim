//! Opt-in VHCI substitution. Bluedroid and its FreeRTOS tasks still execute in the guest.
use crate::bus::{SocBus, DRAM_HIGH, DRAM_LOW};
use emu_core::{Bus, Core};
use std::collections::HashMap;
use xtensa_lx7::{
    decode::{decode, Op},
    Cpu,
};

const INIT: u32 = 48;
const TASK: u32 = 112;
const POLL: u32 = 176;
const CREATED: u32 = 192;
const BUFFER_SIZE: u32 = 2048;
const INVALID_STATE: u32 = 0x103;
const INVALID_ARG: u32 = 0x102;

// Xtensa little-endian, no-transform assembly, relative to controller_init.
// Literals at +4: create, task, name, delay, poll, created, buffer, stack size.
// +48: entry a1,64; l32r a8,create; l32r a10,task; l32r a11,name;
// l32r a12,stack; movi a13,0; movi a14,23; movi a15,0; movi a9,0;
// s32i a9,a1,0; callx8 a8; l32r a8,created; callx8 a8; mov a2,a10; retw.
// +112: entry a1,48; again: l32r a8,poll; callx8 a8; beqz a10,sleep;
// mov a8,a10; l32r a10,buffer; l32i a11,a10,0; addi a10,a10,16;
// callx8 a8; j again; sleep: movi a10,1; l32r a8,delay; callx8 a8; j again.
// +176/+192: entry a1,32; retw. These two entries are host-intercepted.
const CODE: &[u8] = &[
    0x36, 0x81, 0x00, 0x81, 0xf4, 0xff, 0xa1, 0xf4, 0xff, 0xb1, 0xf4, 0xff, 0xc1, 0xf9, 0xff, 0xd2,
    0xa0, 0x00, 0xe2, 0xa0, 0x17, 0xf2, 0xa0, 0x00, 0x92, 0xa0, 0x00, 0x92, 0x61, 0x00, 0xe0, 0x08,
    0x00, 0x81, 0xf1, 0xff, 0xe0, 0x08, 0x00, 0xa0, 0x2a, 0x20, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x36, 0x61, 0x00, 0x81, 0xe8, 0xff, 0xe0, 0x08, 0x00, 0x16, 0x1a, 0x01, 0xa0, 0x8a, 0x20, 0xa1,
    0xe7, 0xff, 0xb2, 0x2a, 0x00, 0xa2, 0xca, 0x10, 0xe0, 0x08, 0x00, 0x06, 0xf9, 0xff, 0xa2, 0xa0,
    0x01, 0x81, 0xdf, 0xff, 0xe0, 0x08, 0x00, 0x06, 0xf6, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x36, 0x41, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x36, 0x41, 0x00, 0x90, 0x00, 0x00,
];

#[derive(Clone, Copy)]
enum Hook {
    Init,
    Created,
    Poll,
    Enable,
    Disable,
    Deinit,
    Status,
    Release,
    Register,
    Available,
    Send,
    PowerGet,
    PowerSet,
}

pub struct Ble {
    pub controller: esp_soc::ble::Controller,
    pub hooks: Vec<u32>,
    kinds: HashMap<u32, Hook>,
    init: u32,
    create: u32,
    delay: u32,
    buffer: u32,
    status: u32,
    callbacks: [u32; 2],
    send_ready: bool,
    task_created: bool,
    pub(crate) original_flash: Option<(usize, Vec<u8>)>,
}

impl Default for Ble {
    fn default() -> Self {
        Self {
            controller: esp_soc::ble::Controller::new(),
            hooks: Vec::new(),
            kinds: HashMap::new(),
            init: 0,
            create: 0,
            delay: 0,
            buffer: 0,
            status: 0,
            callbacks: [0; 2],
            send_ready: false,
            task_created: false,
            original_flash: None,
        }
    }
}

impl Ble {
    pub fn enable(&mut self, symbols: &HashMap<String, u32>) -> Result<(), String> {
        let get = |name: &str| {
            symbols
                .get(name)
                .copied()
                .ok_or_else(|| format!("BLE requires ELF symbol {name}"))
        };
        let init = get("esp_bt_controller_init")?;
        let next = symbols
            .values()
            .copied()
            .filter(|&pc| pc > init)
            .min()
            .unwrap_or(init);
        if init & 3 != 0 || next.saturating_sub(init) < INIT + CODE.len() as u32 {
            return Err("BLE controller init has no room for the guest task trampoline".into());
        }
        let buffer = get("_bt_controller_bss_start")?;
        let end = get("_bt_controller_bss_end")?;
        if !(DRAM_LOW..DRAM_HIGH).contains(&buffer)
            || end > DRAM_HIGH
            || buffer & 3 != 0
            || end.saturating_sub(buffer) < BUFFER_SIZE
        {
            return Err("BLE controller BSS is too small or outside DRAM".into());
        }
        let mut next = Self {
            init,
            buffer,
            create: get("xTaskCreatePinnedToCore")?,
            delay: get("vTaskDelay")?,
            ..Self::default()
        };
        for (name, kind) in [
            ("esp_bt_controller_init", Hook::Init),
            ("esp_bt_controller_enable", Hook::Enable),
            ("esp_bt_controller_get_status", Hook::Status),
            ("esp_vhci_host_register_callback", Hook::Register),
            ("esp_vhci_host_check_send_available", Hook::Available),
            ("esp_vhci_host_send_packet", Hook::Send),
        ] {
            next.kinds.insert(get(name)?, kind);
        }
        for (name, kind) in [
            ("esp_bt_controller_disable", Hook::Disable),
            ("esp_bt_controller_deinit", Hook::Deinit),
            ("esp_bt_controller_mem_release", Hook::Release),
            ("esp_bt_mem_release", Hook::Release),
            ("esp_ble_tx_power_get", Hook::PowerGet),
            ("esp_ble_tx_power_set", Hook::PowerSet),
        ] {
            if let Some(&address) = symbols.get(name) {
                next.kinds.insert(address, kind);
            }
        }
        next.kinds.insert(init + POLL, Hook::Poll);
        next.kinds.insert(init + CREATED, Hook::Created);
        next.hooks = next.kinds.keys().copied().collect();
        *self = next;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.controller = esp_soc::ble::Controller::new();
        self.status = 0;
        self.callbacks = [0; 2];
        self.send_ready = false;
        self.task_created = false;
    }

    pub fn command(&mut self, command: &str, cycles: u64) -> Result<(), String> {
        if self.hooks.is_empty() {
            return Err("BLE requires --ble and the application ELF".into());
        }
        let result = self.controller.command(command);
        self.log(cycles);
        result
    }

    fn log(&mut self, cycles: u64) {
        for line in self.controller.drain_log() {
            eprintln!(
                "[ble] t={:.6}s {line}",
                cycles as f64 / crate::periph::CPU_HZ as f64
            );
        }
    }
}

fn windowed(bus: &mut SocBus, pc: u32) -> bool {
    bus.fetch(pc)
        .is_ok_and(|bytes| decode(pc, bytes).op == Op::Entry)
}

fn install(bus: &mut SocBus) -> Result<(), String> {
    let b = &bus.ble;
    let (init, buffer) = (b.init, b.buffer);
    let literals = [
        b.create,
        init + TASK,
        buffer + 4,
        b.delay,
        init + POLL,
        init + CREATED,
        buffer,
        4096,
    ];
    if !windowed(bus, init) {
        return Err("BLE controller init is not a windowed Xtensa function".into());
    }
    if let Some(offset) = bus.flash_off(init + 4) {
        let len = INIT as usize + CODE.len() - 4;
        if bus.flash_off(init + 4 + len as u32 - 1) != Some(offset + len - 1) {
            return Err("BLE controller init crosses a noncontiguous flash mapping".into());
        }
        if bus.ble.original_flash.is_none() {
            bus.ble.original_flash = Some((offset, bus.flash[offset..offset + len].to_vec()));
        }
    }
    for (i, value) in literals.into_iter().enumerate() {
        bus.load_bytes(init + 4 + i as u32 * 4, &value.to_le_bytes())?;
    }
    bus.load_bytes(init + INIT, CODE)?;
    bus.load_bytes(buffer + 4, b"vhci\0")?;
    Ok(())
}

pub fn intercept(cpu: &mut Cpu, bus: &mut SocBus) -> bool {
    let Some(&hook) = bus.ble.kinds.get(&cpu.pc) else {
        return false;
    };
    let arg = cpu.arg(bus, 0);
    let mut result = 0;
    match hook {
        Hook::Init => {
            if bus.ble.status != 0 {
                result = INVALID_STATE;
            } else if arg == 0 {
                result = INVALID_ARG;
            } else if bus.ble.task_created {
                bus.ble.status = 1;
            } else {
                match install(bus) {
                    Ok(()) => {
                        cpu.flush_caches();
                        cpu.pc = bus.ble.init + INIT;
                        cpu.insn_count += 1;
                        cpu.advance_cycles(1);
                        return true;
                    }
                    Err(error) => {
                        eprintln!("[ble] {error}");
                        result = u32::MAX;
                    }
                }
            }
        }
        Hook::Created => {
            if arg == 1 {
                bus.ble.task_created = true;
                bus.ble.status = 1;
            } else {
                eprintln!("[ble] controller task creation failed: {arg:#x}");
                result = u32::MAX;
            }
        }
        Hook::Enable => {
            if bus.ble.status != 1 {
                result = INVALID_STATE;
            } else if arg != 1 && arg != 3 {
                result = INVALID_ARG;
            } else {
                bus.ble.status = 2;
            }
        }
        Hook::Disable => {
            if bus.ble.status != 2 {
                result = INVALID_STATE;
            } else {
                bus.ble.status = 1;
            }
        }
        Hook::Deinit => {
            if bus.ble.status != 1 {
                result = INVALID_STATE;
            } else {
                bus.ble.status = 0;
                bus.ble.callbacks = [0; 2];
                bus.ble.send_ready = false;
                // ponytail: keep one task per boot; delete it when memory release is modeled.
                bus.ble.controller = esp_soc::ble::Controller::new();
            }
        }
        Hook::Status => result = bus.ble.status,
        // The injected task and packet buffer occupy controller memory for this boot.
        Hook::Release => {
            if bus.ble.status != 0 {
                result = INVALID_STATE;
            }
        }
        Hook::Register => {
            if arg == 0 || arg > u32::MAX - 7 {
                result = INVALID_ARG;
            } else if let (Ok(send), Ok(recv)) =
                (bus.read32_unpriced(arg), bus.read32_unpriced(arg + 4))
            {
                if !windowed(bus, send) || !windowed(bus, recv) {
                    result = INVALID_ARG;
                } else {
                    bus.ble.callbacks = [send, recv];
                    bus.ble.send_ready = true;
                }
            } else {
                result = INVALID_ARG;
            }
        }
        Hook::Available => result = u32::from(bus.ble.status == 2),
        Hook::Send => {
            let len = cpu.arg(bus, 1) as usize;
            if bus.ble.status != 2
                || len == 0
                || len > (BUFFER_SIZE - 16) as usize
                || arg < DRAM_LOW
                || arg
                    .checked_add(len as u32)
                    .is_none_or(|end| end > DRAM_HIGH)
            {
                eprintln!("[ble] rejected invalid VHCI packet pointer, length or controller state");
            } else {
                let packet: Result<Vec<u8>, _> = (0..len)
                    .map(|i| bus.read8_unpriced(arg + i as u32))
                    .collect();
                if let Ok(packet) = packet {
                    if bus.debug.has("ble") {
                        eprintln!("[ble] tx {:02x?}", packet);
                    }
                    bus.ble.controller.send(&packet);
                    bus.ble.send_ready = true;
                    bus.ble.log(bus.cycles);
                }
            }
        }
        Hook::Poll => {
            if bus.ble.status == 2 && bus.ble.callbacks[1] != 0 {
                if let Some(packet) = bus.ble.controller.pop_packet() {
                    let buffer = bus.ble.buffer;
                    if packet.len() <= (BUFFER_SIZE - 16) as usize
                        && bus
                            .load_bytes(buffer, &(packet.len() as u32).to_le_bytes())
                            .is_ok()
                        && bus.load_bytes(buffer + 16, &packet).is_ok()
                    {
                        if bus.debug.has("ble") {
                            eprintln!("[ble] rx {:02x?}", packet);
                        }
                        result = bus.ble.callbacks[1];
                    } else {
                        eprintln!("[ble] rejected oversized or unmapped controller packet");
                    }
                } else if bus.ble.send_ready {
                    bus.ble.send_ready = false;
                    result = bus.ble.callbacks[0];
                }
            }
        }
        Hook::PowerGet => result = 5,
        Hook::PowerSet => {}
    }
    cpu.return_from_stub(bus, result);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use xtensa_lx7::state::ps;

    const BASE: u32 = 0x4008_0000;
    const RETURN: u32 = 0x4008_1000;
    const SEND_CB: u32 = BASE + 0x700;
    const RECV_CB: u32 = BASE + 0x710;

    fn fixture() -> (SocBus, HashMap<String, u32>) {
        let mut bus = SocBus::new(0x400000, [0; 6]);
        let symbols: HashMap<String, u32> = [
            ("esp_bt_controller_init", BASE),
            ("xTaskCreatePinnedToCore", BASE + 0x400),
            ("vTaskDelay", BASE + 0x500),
            ("esp_bt_controller_get_status", BASE + 0x600),
            ("esp_bt_controller_enable", BASE + 0x610),
            ("esp_vhci_host_check_send_available", BASE + 0x620),
            ("esp_vhci_host_send_packet", BASE + 0x630),
            ("esp_vhci_host_register_callback", BASE + 0x640),
            ("_bt_controller_bss_start", 0x3ffc_7104),
            ("_bt_controller_bss_end", 0x3ffc_7c7c),
        ]
        .into_iter()
        .map(|(n, a)| (n.into(), a))
        .collect();
        for &pc in symbols.values().filter(|&&pc| pc >= BASE) {
            bus.load_bytes(pc, &[0x36, 0x41, 0, 0x90, 0, 0]).unwrap();
        }
        for pc in [SEND_CB, RECV_CB] {
            bus.load_bytes(pc, &[0x36, 0x41, 0, 0x90, 0, 0]).unwrap();
        }
        bus.ble.enable(&symbols).unwrap();
        (bus, symbols)
    }

    fn caller(pc: u32, arg: u32) -> Cpu {
        let mut cpu = Cpu::new(0);
        cpu.ps = ps::WOE | ps::UM | (2 << ps::CALLINC_SHIFT);
        cpu.windowbase = 0;
        cpu.windowstart = 1;
        cpu.pc = pc;
        cpu.set_ar(1, 0x3ffe_0000);
        cpu.set_ar(8, (2 << 30) | (RETURN & 0x3fff_ffff));
        cpu.set_ar(10, arg);
        cpu
    }

    #[test]
    fn trampoline_creates_guest_task_with_windowed_abi_and_returns_esp_ok() {
        let (mut bus, _) = fixture();
        let mut cpu = caller(BASE, DRAM_LOW);
        assert!(intercept(&mut cpu, &mut bus));
        let mut created = false;
        for _ in 0..100 {
            if cpu.pc == RETURN {
                break;
            }
            if cpu.pc == bus.ble.create {
                let args: Vec<u32> = (0..6).map(|n| cpu.arg(&mut bus, n)).collect();
                assert_eq!(args, [BASE + TASK, bus.ble.buffer + 4, 4096, 0, 23, 0]);
                assert_eq!(
                    bus.read32_unpriced(cpu.get_ar(1)).unwrap(),
                    0,
                    "core ID is seventh stack argument"
                );
                cpu.return_from_stub(&mut bus, 1);
                created = true;
            } else if !intercept(&mut cpu, &mut bus) {
                assert!(cpu.step(&mut bus).trap().is_none());
            }
        }
        assert!(created);
        assert_eq!(cpu.pc, RETURN);
        assert_eq!(cpu.get_ar(10), 0);
        assert_eq!(cpu.windowbase, 0);
        assert_eq!(bus.ble.status, 1);
        assert!(bus.ble.task_created);
        let hooks = bus.ble.hooks.clone();
        bus.ble.reset();
        assert_eq!(bus.ble.status, 0);
        assert!(!bus.ble.task_created);
        assert_eq!(bus.ble.hooks, hooks);
    }

    #[test]
    fn guest_task_calls_receive_and_send_ready_then_delays() {
        let (mut bus, _) = fixture();
        install(&mut bus).unwrap();
        bus.ble.status = 2;
        bus.ble.callbacks = [SEND_CB, RECV_CB];
        bus.ble.send_ready = true;
        bus.ble.controller.send(&[1, 3, 12, 0]);
        let mut cpu = caller(BASE + TASK, 0);
        let (mut received, mut ready, mut delayed) = (false, false, false);
        for _ in 0..100 {
            if cpu.pc == RECV_CB {
                let (ptr, len) = (cpu.arg(&mut bus, 0), cpu.arg(&mut bus, 1));
                let bytes: Vec<u8> = (0..len)
                    .map(|i| bus.read8_unpriced(ptr + i).unwrap())
                    .collect();
                assert_eq!(bytes, [4, 14, 4, 1, 3, 12, 0]);
                cpu.return_from_stub(&mut bus, 0);
                received = true;
            } else if cpu.pc == SEND_CB {
                cpu.return_from_stub(&mut bus, 0);
                ready = true;
            } else if cpu.pc == bus.ble.delay {
                assert_eq!(cpu.arg(&mut bus, 0), 1);
                delayed = true;
                break;
            } else if !intercept(&mut cpu, &mut bus) {
                assert!(cpu.step(&mut bus).trap().is_none());
            }
        }
        assert!(received && ready && delayed);
    }

    #[test]
    fn invalid_guest_buffers_and_missing_symbols_are_rejected() {
        let (mut bus, mut symbols) = fixture();
        let mut cpu = caller(symbols["esp_vhci_host_register_callback"], 0xffff_fffc);
        assert!(intercept(&mut cpu, &mut bus));
        assert_eq!(cpu.get_ar(10), INVALID_ARG);
        let table = 0x3ff9_1000;
        bus.load_bytes(table, &SEND_CB.to_le_bytes()).unwrap();
        bus.load_bytes(table + 4, &RECV_CB.to_le_bytes()).unwrap();
        let mut cpu = caller(symbols["esp_vhci_host_register_callback"], table);
        assert!(intercept(&mut cpu, &mut bus));
        assert_eq!(
            cpu.get_ar(10),
            0,
            "a static callback table can live in DROM"
        );
        assert_eq!(bus.ble.callbacks, [SEND_CB, RECV_CB]);
        symbols.remove("vTaskDelay");
        assert!(bus.ble.enable(&symbols).unwrap_err().contains("vTaskDelay"));
    }

    #[test]
    fn machine_dispatches_chip_hooks_as_function_boundaries() {
        let (bus, symbols) = fixture();
        let mut m = esp_soc::Machine::<crate::Esp32>::new([0; 6], bus);
        m.quantum = 1;
        m.bus.ble.status = 2;
        m.cores[0] = caller(symbols["esp_bt_controller_get_status"], 0);
        assert!(matches!(m.run(1), esp_soc::Stop::MaxInsns));
        assert_eq!(m.cores[0].pc, RETURN);
        assert_eq!(m.cores[0].get_ar(10), 2);
    }
    #[test]
    fn reboot_restores_controller_flash_even_after_mmu_mapping_changes() {
        let (mut bus, symbols) = fixture();
        let init = crate::bus::IBUS_LOW;
        let symbols = symbols
            .into_iter()
            .map(|(name, address)| {
                (
                    name,
                    if address >= BASE {
                        address - BASE + init
                    } else {
                        address
                    },
                )
            })
            .collect();
        bus.ble.enable(&symbols).unwrap();
        let slot = 64 + ((init - 0x4000_0000) >> 16) as usize;
        bus.mmu[0][slot] = 0;
        let mut original: Vec<u8> = (0..INIT as usize + CODE.len()).map(|i| i as u8).collect();
        original[..3].copy_from_slice(&[0x36, 0x41, 0]);
        bus.load_bytes(init, &original).unwrap();
        install(&mut bus).unwrap();
        assert_ne!(&bus.flash[..original.len()], &original);
        bus.mmu[0][slot] = 1;
        esp_soc::SocBus::reboot(&mut bus, [0; 6]);
        assert_eq!(&bus.flash[..original.len()], &original);
        assert!(bus.ble.original_flash.is_none());
        assert!(!bus.ble.hooks.is_empty());
    }
}

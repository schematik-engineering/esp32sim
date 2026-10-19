//! Strict, single-HP-core ESP32-S31 reset probe. Not native-ROM-to-application support.
//! No guessed MMIO defaults, CLIC delivery, MMU, cache timing, FPU, or secondary core.
use emu_core::{Bus, Core, Fault, StepKind};
use riscv_rv32::{
    decode::{decode, Op},
    Cpu,
};

pub const CHIP: &str = "esp32s31";
// ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
// components/soc/esp32s31/include/soc/soc.h:138-217. All ranges half-open.
pub const RESET: u32 = 0x2f80_0000;
pub const ROM: (u32, u32) = (RESET, 0x2f85_0000);
pub const HP_SRAM: (u32, u32) = (0x2f00_0000, 0x2f08_0000);
pub const LP_SRAM: (u32, u32) = (0x2e00_0000, 0x2e00_8000);
pub const FLASH: (u32, u32) = (0x4000_0000, 0x4400_0000);
pub const PSRAM: (u32, u32) = (0x5000_0000, 0x5400_0000);
// These are ADDITIVE offsets, not alias base addresses. Also see the pinned IDF
// components/esp_hw_support/port/esp32s31/cpu_region_protect.c:90-101.
pub const FLASH_NC_OFFSET: u32 = 0x6000_0000;
pub const PSRAM_NC_OFFSET: u32 = 0x7000_0000;

// ESP-IDF 9a97f6c54ec638111ce55cd36581b3c192f15207,
// components/soc/esp32s31/register/soc/reg_base.h:84 and
// lp_clkrst_reg.h:291-339: cause [6:1], POR = 1, flag reset = 0.
// This probe chooses power-on, not the M0 USB-UART reset (cause 0x17).
pub const HPCORE0_RESET_CAUSE: u32 = 0x2070_1030;
const POWER_ON_RESET: u32 = 1 << 1;
// Same IDF, components/soc/esp32s31/include/soc/clic_reg.h:17-38:
// NVBITS is RO 1, NMBITS is RO 0, NLBITS is RW [4:1], reset 0.
pub const CLIC_CONFIG: u32 = 0x1080_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Rom,
    HpSram,
    LpSram,
    Flash,
    Psram,
    Mmio,
}

/// Virtual aperture classification only; does not imply a configured MMU or backing.
pub fn locate(addr: u32) -> Option<(Region, usize)> {
    for (region, (lo, hi)) in [
        (Region::Rom, ROM),
        (Region::HpSram, HP_SRAM),
        (Region::LpSram, LP_SRAM),
        (Region::Flash, FLASH),
        (Region::Psram, PSRAM),
        (
            Region::Flash,
            (FLASH.0 + FLASH_NC_OFFSET, FLASH.1 + FLASH_NC_OFFSET),
        ),
        (
            Region::Psram,
            (PSRAM.0 + PSRAM_NC_OFFSET, PSRAM.1 + PSRAM_NC_OFFSET),
        ),
    ] {
        if (lo..hi).contains(&addr) {
            return Some((region, (addr - lo) as usize));
        }
    }
    // Apertures only: accesses fault except the explicitly audited registers below.
    if (0x1000_0000..0x2000_0000).contains(&addr) || (0x2010_0000..0x2e00_0000).contains(&addr) {
        return Some((Region::Mmio, 0));
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessFault {
    pub address: u32,
    pub width: usize,
    pub write: bool,
    pub fault: Fault,
}

pub struct S31Bus {
    rom: Vec<u8>,
    rom_present: Vec<bool>,
    hp: Vec<u8>,
    lp: Vec<u8>,
    reset_cause: u32,
    clic_config: u32,
    /// First fault only, including width and direction; unknown MMIO never reads as zero.
    pub fault: Option<AccessFault>,
}

impl Default for S31Bus {
    fn default() -> Self {
        Self::new()
    }
}
impl S31Bus {
    pub fn new() -> Self {
        Self {
            rom: vec![0; (ROM.1 - ROM.0) as usize],
            rom_present: vec![false; (ROM.1 - ROM.0) as usize],
            hp: vec![0; (HP_SRAM.1 - HP_SRAM.0) as usize],
            lp: vec![0; (LP_SRAM.1 - LP_SRAM.0) as usize],
            reset_cause: POWER_ON_RESET,
            clic_config: 1,
            fault: None,
        }
    }

    /// Load only ROM-addressed PT_LOAD contents. No host .eh_frame at zero, no
    /// direct-app loading, and no preinitialization of RAM sections: ROM does that.
    pub fn load_rom(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.get(18..20) != Some(&243u16.to_le_bytes()) {
            return Err("need RISC-V ELF".into());
        }
        let elf = esp_soc::elf::parse(bytes)?;
        if elf.entry != RESET {
            return Err("S31 ROM entry must be 0x2f800000".into());
        }
        let mut image = vec![0; self.rom.len()];
        let mut present = vec![false; image.len()];
        for seg in &elf.segments {
            if seg.data.is_empty() || seg.vaddr == 0 {
                continue;
            }
            let off = seg.vaddr.checked_sub(ROM.0).ok_or("non-ROM load segment")? as usize;
            off.checked_add(seg.memsz as usize)
                .filter(|&end| end <= image.len())
                .ok_or("ROM segment exceeds aperture")?;
            let data_end = off + seg.data.len();
            image[off..data_end].copy_from_slice(&seg.data);
            // A missing ROM file byte isn't writable zero-filled RAM.
            present[off..data_end].fill(true);
        }
        if !present[..4].iter().all(|&b| b) {
            return Err("ROM reset instruction missing".into());
        }
        self.rom = image;
        self.rom_present = present;
        self.fault = None;
        Ok(())
    }

    /// Supplement only the public ELF's data hole with caller-provided ROM reads.
    /// Fixed range prevents accidentally treating an arbitrary file as a full ROM.
    /// No zero padding, writable ROM, or replacement of ELF-backed bytes.
    pub fn load_rom_data(&mut self, bytes: &[u8]) -> Result<(), String> {
        const START: usize = 0x3f700;
        const END: usize = 0x48000;
        if bytes.len() != END - START {
            return Err(
                "S31 ROM data must cover [0x2f83f700,0x2f848000), exactly 35072 bytes".into(),
            );
        }
        if !self.rom_present[..4].iter().all(|&b| b) {
            return Err("load the public S31 ROM ELF before its data supplement".into());
        }
        if self.rom_present[START..END].iter().any(|&b| b) {
            return Err("S31 ROM data would overwrite already loaded bytes".into());
        }
        self.rom[START..END].copy_from_slice(bytes);
        self.rom_present[START..END].fill(true);
        Ok(())
    }

    fn fail<T>(
        &mut self,
        address: u32,
        width: usize,
        write: bool,
        fault: Fault,
    ) -> Result<T, Fault> {
        self.fault.get_or_insert(AccessFault {
            address,
            width,
            write,
            fault,
        });
        Err(fault)
    }

    fn span(&mut self, addr: u32, n: usize, write: bool) -> Result<(Region, usize), Fault> {
        let Some((region, off)) = locate(addr) else {
            return self.fail(addr, n, write, Fault::Unmapped);
        };
        let Some(last) = addr.checked_add(n as u32 - 1) else {
            return self.fail(addr, n, write, Fault::Unmapped);
        };
        if locate(last) != Some((region, off + n - 1)) {
            return self.fail(addr, n, write, Fault::Unmapped);
        }
        if write && region == Region::Rom {
            return self.fail(addr, n, write, Fault::Prohibited);
        }
        match region {
            Region::Rom if self.rom_present[off..off + n].iter().all(|&b| b) => Ok((region, off)),
            Region::HpSram | Region::LpSram => Ok((region, off)),
            // Cache windows are declared but deliberately NOT identity-mapped to
            // flash storage. Their MMU/reset configuration is not implemented.
            _ => self.fail(addr, n, write, Fault::Unmapped),
        }
    }
    fn read<const N: usize>(&mut self, addr: u32) -> Result<[u8; N], Fault> {
        let (r, o) = self.span(addr, N, false)?;
        let mem = match r {
            Region::Rom => &self.rom,
            Region::HpSram => &self.hp,
            Region::LpSram => &self.lp,
            _ => unreachable!(),
        };
        Ok(mem[o..o + N].try_into().unwrap())
    }
    fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), Fault> {
        let (r, o) = self.span(addr, bytes.len(), true)?;
        let mem = match r {
            Region::HpSram => &mut self.hp,
            Region::LpSram => &mut self.lp,
            _ => unreachable!(),
        };
        mem[o..o + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

impl Bus for S31Bus {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        Ok(self.read::<1>(a)?[0])
    }
    fn read16(&mut self, a: u32) -> Result<u16, Fault> {
        Ok(u16::from_le_bytes(self.read(a)?))
    }
    fn read32(&mut self, a: u32) -> Result<u32, Fault> {
        match a {
            HPCORE0_RESET_CAUSE => return Ok(self.reset_cause),
            CLIC_CONFIG => return Ok(self.clic_config),
            _ => {}
        }
        Ok(u32::from_le_bytes(self.read(a)?))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        self.write(a, &[v])
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), Fault> {
        self.write(a, &v.to_le_bytes())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), Fault> {
        match a {
            HPCORE0_RESET_CAUSE => {
                // WT clear bits; cause and flag are RO, reserved bits ignored.
                if v & (1 << 30) != 0 {
                    self.reset_cause &= !0x7e;
                }
                if v & (1 << 31) != 0 {
                    self.reset_cause &= !1;
                }
                return Ok(());
            }
            CLIC_CONFIG => {
                // The header documents at most eight level bits, but not the
                // WARL behavior above eight. Fail closed instead of guessing it.
                if ((v >> 1) & 0xf) > 8 {
                    return self.fail(a, 4, true, Fault::Prohibited);
                }
                self.clic_config = (v & 0x1e) | 1;
                return Ok(());
            }
            _ => {}
        }
        self.write(a, &v.to_le_bytes())
    }
    fn fetch(&mut self, pc: u32) -> Result<[u8; 4], Fault> {
        // Do not fault a valid compressed instruction at an aperture's final halfword.
        let lo = self.read::<2>(pc)?;
        if lo[0] & 3 != 3 {
            return Ok([lo[0], lo[1], 0, 0]);
        }
        self.read(pc)
    }
    fn note_code_page(&mut self, _: u32) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    Access(AccessFault),
    Opcode,
    Csr(u32),
    Trap(emu_core::Trap),
    Waiting,
    Budget,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    pub pc: u32,
    pub raw: u32,
    pub retired: u64,
    pub reason: StopReason,
}

pub struct Probe {
    pub cpu: Cpu,
    pub bus: S31Bus,
}
impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}
impl Probe {
    pub fn new() -> Self {
        let mut cpu = Cpu::new_rv32imac();
        cpu.pc = RESET;
        Self {
            cpu,
            bus: S31Bus::new(),
        }
    }
    pub fn run(&mut self, budget: u64) -> Stop {
        for _ in 0..budget {
            let pc = self.cpu.pc;
            let bytes = match self.bus.fetch(pc) {
                Ok(b) => b,
                Err(_) => {
                    return Stop {
                        pc,
                        raw: 0,
                        retired: self.cpu.retired_count,
                        reason: StopReason::Access(self.bus.fault.unwrap()),
                    }
                }
            };
            let insn = decode(pc, bytes);
            let stop = |reason, retired| Stop {
                pc,
                raw: insn.raw,
                retired,
                reason,
            };
            if insn.is_illegal() {
                return stop(StopReason::Opcode, self.cpu.retired_count);
            }
            if matches!(
                insn.op,
                Op::Csrrw | Op::Csrrs | Op::Csrrc | Op::Csrrwi | Op::Csrrsi | Op::Csrrci
            ) {
                // Only the reset-prefix CSRs have audited semantics for this probe.
                // Do not silently inherit the C3/C6 generic unknown-CSR readback.
                // Standard mcounteren CY/TM/IR bits only. Lower privilege is not
                // implemented. mtvec stores mode 1; no interrupt is injected and
                // no CLIC dispatch is claimed by this reset-prefix probe.
                if !matches!(insn.imm as u32, 0x300 | 0x305 | 0x306 | 0xf14) {
                    return stop(StopReason::Csr(insn.imm as u32), self.cpu.retired_count);
                }
            }
            let outcome = self.cpu.step(&mut self.bus);
            if let Some(v) = self.cpu.csr_other.get_mut(&0x306) {
                *v &= 7;
            }
            if let Some(f) = self.bus.fault {
                return stop(StopReason::Access(f), self.cpu.retired_count);
            }
            if let StepKind::TrapBefore(t) | StepKind::TrapDuring(t) = outcome.kind {
                return stop(StopReason::Trap(t), self.cpu.retired_count);
            }
            if self.cpu.waiting {
                return stop(StopReason::Waiting, self.cpu.retired_count);
            }
        }
        Stop {
            pc: self.cpu.pc,
            raw: 0,
            retired: self.cpu.retired_count,
            reason: StopReason::Budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_cause_mapping_and_write_triggers() {
        let mut b = S31Bus::new();
        assert_eq!(b.read32(0x20701030), Ok(2));
        b.write32(0x20701030, 0x3fff_ffff).unwrap();
        assert_eq!(b.read32(0x20701030), Ok(2)); // RO cause, no clear trigger
        b.reset_cause |= 1; // Exercise flag clearing independently of cause.
        b.write32(0x20701030, 1 << 31).unwrap();
        assert_eq!(b.read32(0x20701030), Ok(2));
        b.reset_cause |= 1;
        b.write32(0x20701030, 1 << 30).unwrap();
        assert_eq!(b.read32(0x20701030), Ok(1));
        b.write32(0x20701030, 1 << 31).unwrap();
        assert_eq!(b.read32(0x20701030), Ok(0));
    }
    #[test]
    fn clic_config_mapping_and_fields() {
        let mut b = S31Bus::new();
        assert_eq!(b.read32(0x10800000), Ok(1));
        for levels in 0..=8 {
            b.write32(0x10800000, 0xffff_ffe0 | (levels << 1)).unwrap();
            assert_eq!(b.read32(0x10800000), Ok(1 | (levels << 1)));
        }
        assert_eq!(b.write32(0x10800000, 9 << 1), Err(Fault::Prohibited));
        assert_eq!(b.read32(0x10800000), Ok(17));
    }
    #[test]
    fn mmio_unsupported_neighbors_widths_and_fetch_fail_closed() {
        let mut b = S31Bus::new();
        for a in [0x2070102c, 0x20701034, 0x10800004, 0x10810000] {
            assert_eq!(b.read32(a), Err(Fault::Unmapped));
            assert_eq!(b.write32(a, 0), Err(Fault::Unmapped));
        }
        for a in [0x20701030, 0x10800000] {
            assert_eq!(b.read8(a), Err(Fault::Unmapped));
            assert_eq!(b.read16(a), Err(Fault::Unmapped));
            assert_eq!(b.read32(a + 1), Err(Fault::Unmapped));
            assert_eq!(b.write8(a, 0), Err(Fault::Unmapped));
            assert_eq!(b.write16(a, 0), Err(Fault::Unmapped));
            assert_eq!(b.fetch(a), Err(Fault::Unmapped));
        }
    }
    #[test]
    fn reset_prefix_standard_csr_instructions() {
        // Standard Zicsr encodings, not new S31 extension decoding. All six
        // operations exercise explicit mcounteren storage; unknown CSRs stop.
        let mut p = Probe::new();
        p.cpu.pc = HP_SRAM.0;
        p.cpu.x[1] = 0xffff_fffd;
        for (i, (funct, source, expected_old, expected_new)) in [
            (1, 1, 0, 5), // csrrw
            (2, 1, 5, 5), // csrrs
            (3, 1, 5, 0), // csrrc
            (5, 7, 0, 7), // csrrwi
            (7, 3, 7, 4), // csrrci
            (6, 2, 4, 6), // csrrsi
        ]
        .into_iter()
        .enumerate()
        {
            let pc = HP_SRAM.0 + i as u32 * 4;
            let raw = (0x306 << 20) | (source << 15) | (funct << 12) | (2 << 7) | 0x73;
            p.bus.write32(pc, raw).unwrap();
            assert_eq!(p.run(1).reason, StopReason::Budget);
            assert_eq!(p.cpu.x[2], expected_old);
            assert_eq!(p.cpu.read_csr(0x306), expected_new);
            assert_eq!(p.cpu.retired_count, i as u64 + 1);
        }
        // The ROM writes mtvec in standard mode 1; storage is not delivery.
        p.cpu.x[1] = 0x2f801701;
        p.bus.write32(p.cpu.pc, 0x30509073).unwrap();
        assert_eq!(p.run(1).reason, StopReason::Budget);
        assert_eq!(p.cpu.mtvec, 0x2f801701);
    }
    #[test]
    fn map_pins_every_aperture_and_boundary() {
        for (region, lo, hi) in [
            (Region::Rom, 0x2f800000, 0x2f850000),
            (Region::HpSram, 0x2f000000, 0x2f080000),
            (Region::LpSram, 0x2e000000, 0x2e008000),
            (Region::Flash, 0x40000000, 0x44000000),
            (Region::Psram, 0x50000000, 0x54000000),
            (Region::Flash, 0xa0000000, 0xa4000000),
            (Region::Psram, 0xc0000000, 0xc4000000),
        ] {
            assert_eq!(locate(lo), Some((region, 0)));
            assert_eq!(locate(hi - 1), Some((region, (hi - lo - 1) as usize)));
            assert_eq!(locate(hi), None);
            assert_ne!(locate(lo - 1).map(|x| x.0), Some(region));
        }
        for a in [
            0x10000000, 0x1fffffff, 0x20100000, 0x2c000000, 0x2c01ffff, 0x2dffffff,
        ] {
            assert_eq!(locate(a), Some((Region::Mmio, 0)));
        }
        for a in [0, 0x20000000, 0x60000000, 0x70000000, 0xffffffff] {
            assert_eq!(locate(a), None);
        }
        assert_eq!(CHIP, "esp32s31");
        assert_eq!(RESET, 0x2f800000);
    }
    #[test]
    fn ram_widths_edges_and_no_cross_boundary_write() {
        let mut b = S31Bus::new();
        for (lo, hi) in [(0x2f000000, 0x2f080000), (0x2e000000, 0x2e008000)] {
            assert_eq!(b.read32(lo).unwrap(), 0);
            b.write32(lo, 0x12345678).unwrap();
            assert_eq!(b.read8(lo + 1).unwrap(), 0x56);
            b.write16(hi - 2, 0xabcd).unwrap();
            assert_eq!(b.read16(hi - 2).unwrap(), 0xabcd);
            assert!(b.write32(hi - 2, 0).is_err());
            assert_eq!(b.read16(hi - 2).unwrap(), 0xabcd);
        }
    }
    #[test]
    fn holes_mmio_windows_and_rom_writes_fail_closed() {
        let mut b = S31Bus::new();
        assert_eq!(b.write8(RESET, 1), Err(Fault::Prohibited));
        b.fault = None;
        assert!(b.fetch(RESET).is_err());
        b.fault = None;
        assert_eq!(b.read32(0x2070102c), Err(Fault::Unmapped));
        assert_eq!(
            b.fault,
            Some(AccessFault {
                address: 0x2070102c,
                width: 4,
                write: false,
                fault: Fault::Unmapped
            })
        );
        for a in [0x40000000, 0x50000000, 0xa0000000, 0xc0000000] {
            assert!(b.read32(a).is_err());
        }
    }
    #[test]
    fn compressed_fetch_at_end_and_strict_opcode_stop() {
        let mut p = Probe::new();
        p.bus.rom[..2].copy_from_slice(&[0, 0]);
        p.bus.rom_present[..2].fill(true);
        assert_eq!(
            p.run(10),
            Stop {
                pc: RESET,
                raw: 0,
                retired: 0,
                reason: StopReason::Opcode
            }
        );
        p.cpu.pc = HP_SRAM.1 - 2;
        p.bus.write16(HP_SRAM.1 - 2, 0x0001).unwrap();
        assert_eq!(p.bus.fetch(HP_SRAM.1 - 2).unwrap(), [1, 0, 0, 0]);
    }
    #[test]
    fn unknown_csr_does_not_use_generic_readback() {
        let mut p = Probe::new();
        p.cpu.pc = HP_SRAM.0;
        p.bus.write32(HP_SRAM.0, 0x3073d073).unwrap();
        assert_eq!(p.run(10).reason, StopReason::Csr(0x307));
        assert_eq!(p.cpu.retired_count, 0);
    }
    fn rom_fixture() -> Vec<u8> {
        // Minimal ELF32 with two ROM PT_LOADs and host metadata at address zero.
        // Reset-prefix bytes transcribed from the pinned public ROM, not a ROM blob.
        let code: [u32; 6] = [
            0x30001073, 0xf14022f3, 0x02029363, 0x20701337, 0x03030313, 0x00032303,
        ];
        let mut elf = vec![0u8; 52 + 3 * 32 + 4 + 24 + 4];
        elf[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        elf[18..20].copy_from_slice(&243u16.to_le_bytes());
        elf[24..28].copy_from_slice(&0x2f800000u32.to_le_bytes());
        elf[28..32].copy_from_slice(&52u32.to_le_bytes());
        elf[42..44].copy_from_slice(&32u16.to_le_bytes());
        elf[44..46].copy_from_slice(&3u16.to_le_bytes());
        for (i, addr, offset, size) in [
            (0, 0x2f800000u32, 148u32, 4u32),
            (1, 0x2f801710, 152, 24),
            (2, 0, 176, 4),
        ] {
            let p = 52 + i * 32;
            for (o, v) in [
                (0, 1u32),
                (4, offset),
                (8, addr),
                (12, addr),
                (16, size),
                (20, size),
                (24, 5),
            ] {
                elf[p + o..p + o + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        elf[148..152].copy_from_slice(&0x7100106fu32.to_le_bytes());
        for (i, word) in code.iter().enumerate() {
            elf[152 + i * 4..156 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        elf
    }
    #[test]
    fn loader_and_reset_prefix_stop_without_external_rom() {
        let elf = rom_fixture();
        let mut p = Probe::new();
        p.bus.load_rom(&elf).unwrap();
        assert_eq!(
            p.run(1000),
            Stop {
                pc: 0x2f801728,
                raw: 0,
                retired: 7,
                reason: StopReason::Access(AccessFault {
                    address: 0x2f801728,
                    width: 2,
                    write: false,
                    fault: Fault::Unmapped
                })
            }
        );
        assert!(p.bus.read32(0).is_err());
        assert!(p.bus.read32(0x2f800004).is_err()); // ELF hole, not executable zeros
        assert_eq!(p.bus.read32(0x2f800000).unwrap(), 0x7100106f);
        assert_eq!(p.bus.write32(0x2f800000, 0), Err(Fault::Prohibited));
    }
    #[test]
    fn invalid_rom_is_rejected_without_overwriting_loaded_image() {
        let elf = rom_fixture();
        let mut b = S31Bus::new();
        b.load_rom(&elf).unwrap();
        for (offset, value) in [(18, 0u32), (24, 0x40000000), (60, 0x2f84ffff)] {
            let mut bad = elf.clone();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(b.load_rom(&bad).is_err());
            assert_eq!(b.read32(0x2f800000).unwrap(), 0x7100106f);
        }
        assert!(b.load_rom(b"not ELF").is_err());
    }
    #[test]
    fn rom_data_exact_range_readonly_and_atomic_validation() {
        // Synthetic pattern for loader boundaries, NOT silicon ROM contents.
        let data: Vec<u8> = (0..0x8900).map(|i| (i % 251) as u8).collect();
        let mut b = S31Bus::new();
        assert!(b.load_rom_data(&data).is_err());
        b.load_rom(&rom_fixture()).unwrap();
        for bad in [&data[..0x88ff], &[0u8; 0x8901][..]] {
            assert!(b.load_rom_data(bad).is_err());
            assert_eq!(b.read32(0x2f83f700), Err(Fault::Unmapped));
        }
        b.fault = None;
        assert_eq!(b.load_rom_data(&data), Ok(()));
        for (i, &byte) in data.iter().enumerate() {
            assert_eq!(b.read8(0x2f83f700 + i as u32), Ok(byte));
        }
        assert_eq!(b.read32(0x2f83f700), Ok(u32::from_le_bytes([0, 1, 2, 3])));
        assert_eq!(b.read8(0x2f83f6ff), Err(Fault::Unmapped));
        assert_eq!(b.read8(0x2f848000), Err(Fault::Unmapped));
        assert_eq!(b.write32(0x2f83f700, 0), Err(Fault::Prohibited));
        assert!(b.load_rom_data(&vec![0xff; 0x8900]).is_err());
        assert_eq!(b.read32(0x2f83f700), Ok(0x03020100));
        // A new ELF load clears supplements rather than retaining stale bytes.
        b.load_rom(&rom_fixture()).unwrap();
        assert_eq!(b.read32(0x2f83f700), Err(Fault::Unmapped));
    }
    #[test]
    #[ignore = "set S31_ROM and S31_ROM_DATA to the pinned ELF and local ROM dump"]
    fn external_rom_data_stop() {
        let rom = std::fs::read(std::env::var_os("S31_ROM").expect("set S31_ROM")).unwrap();
        let data =
            std::fs::read(std::env::var_os("S31_ROM_DATA").expect("set S31_ROM_DATA")).unwrap();
        let mut p = Probe::new();
        p.bus.load_rom(&rom).unwrap();
        p.bus.load_rom_data(&data).unwrap();
        assert_eq!(
            p.run(1_000_000),
            Stop {
                pc: 0x2f83e838,
                raw: 0xbdc79073,
                retired: 6331,
                reason: StopReason::Csr(0xbdc),
            }
        );
    }
    #[test]
    #[ignore = "set S31_ROM to the pinned public esp32s31_rev0_rom.elf"]
    fn external_pinned_rom_first_stop() {
        let rom = std::fs::read(std::env::var_os("S31_ROM").expect("set S31_ROM")).unwrap();
        let mut p = Probe::new();
        p.bus.load_rom(&rom).unwrap();
        assert_eq!(p.bus.read32(0).err(), Some(Fault::Unmapped));
        p.bus.fault = None;
        assert_eq!(
            p.run(1000),
            Stop {
                pc: 0x2f801812,
                raw: 0x0007a283,
                retired: 44,
                reason: StopReason::Access(AccessFault {
                    address: 0x2f83f700,
                    width: 4,
                    write: false,
                    fault: Fault::Unmapped
                })
            }
        );
    }
}
pub mod isa_zb;

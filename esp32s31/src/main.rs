use std::io::{self, BufRead};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--decode-stdin" {
        // ISA audit helper: PC RAW (hex), one record in/out; uses the actual C3/C6 decoder.
        for line in io::stdin().lock().lines() {
            let line = line?;
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() != 2 {
                return Err("expected PC RAW hexadecimal pairs".into());
            }
            let pc = u32::from_str_radix(fields[0], 16)?;
            let raw = u32::from_str_radix(fields[1], 16)?;
            let i = riscv_rv32::decode::decode(pc, raw.to_le_bytes());
            println!("{pc:08x}\t{raw:08x}\t{}\t{:?}\t{:?}", i.len, i.op, i.comp);
        }
        return Ok(());
    }
    if !matches!(args.len(), 7 | 9)
        || (args.len() == 9 && args[7] != "--rom-data")
        || args[1] != "--chip"
        || args[2] != esp32s31::CHIP
        || args[3] != "--rom"
        || args[5] != "--max-instructions"
    {
        return Err("usage: esp32sim-s31 --chip esp32s31 --rom ROM.elf --max-instructions N [--rom-data FILE] (or --decode-stdin)".into());
    }
    let budget = args[6].parse::<u64>()?;
    if budget == 0 {
        return Err("instruction budget must be nonzero".into());
    }
    let mut p = esp32s31::Probe::new();
    p.bus.load_rom(&std::fs::read(&args[4])?)?;
    if args.len() == 9 {
        let data = std::fs::read(&args[8]).map_err(|e| {
            format!("cannot read S31 ROM data supplement: {e}; run s31/dump-rom-data.py and pass --rom-data FILE")
        })?;
        p.bus.load_rom_data(&data)?;
    }
    let s = p.run(budget);
    match s.reason {
        esp32s31::StopReason::Access(f) => println!("[s31] chip={} pc=0x{:08x} opcode=0x{:08x} retired={} stop=access address=0x{:08x} width={} write={} fault={:?}", esp32s31::CHIP, s.pc, s.raw, s.retired, f.address, f.width, f.write, f.fault),
        reason => println!("[s31] chip={} pc=0x{:08x} opcode=0x{:08x} retired={} stop={:?}", esp32s31::CHIP, s.pc, s.raw, s.retired, reason),
    }
    // An unsupported stop is this probe's result, not a claim that firmware booted.
    Ok(())
}

#!/usr/bin/env python3
"""Serial assertion-killed ISA mutations; restore exact source bytes in finally."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

MUTANTS = [('rlist-order',
  '[1, 8, 9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27]',
  '[8, 1, 9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27]'),
 ('stack-base', '.div_ceil(16) * 16', '.div_ceil(16) * 32'),
 ('rlist15', 'if rlist == 15 { 13 }', 'if rlist == 15 { 12 }'),
 ('spimm', 'u32::from((raw >> 2) & 3) * 16', 'u32::from((raw >> 2) & 3) * 8'),
 ('popret-vs-pop', '0xb802 | 0xba02 =>', '0xb802 | 0xbe02 =>'),
 ('popretz-as-pop', '0xb802 | 0xba02 =>', '0xb802 | 0xba02 | 0xbc02 =>'),
 ('pop-return',
  'cpu.pc = cpu.pc.wrapping_add(2);',
  'cpu.pc = if insn.op == Pop { x[1] & !1 } else { cpu.pc.wrapping_add(2) };'),
 ('lbu-sign',
  'u32::from(bus.read8(address).map_err(|e| fault(1, false, e))?)',
  'bus.read8(address).map_err(|e| fault(1, false, e))? as i8 as i32 as u32'),
 ('lhu-sign',
  'u32::from(bus.read16(address).map_err(|e| fault(2, false, e))?)',
  'bus.read16(address).map_err(|e| fault(2, false, e))? as i16 as i32 as u32'),
 ('lh-zero',
  'bus.read16(address).map_err(|e| fault(2, false, e))? as i16 as i32 as u32',
  'u32::from(bus.read16(address).map_err(|e| fault(2, false, e))?)'),
 ('zextb-sign',
  'ZextB => x[usize::from(insn.r1)] = a & 0xff',
  'ZextB => x[usize::from(insn.r1)] = a as i8 as i32 as u32'),
 ('sextb-zero',
  'SextB => x[usize::from(insn.r1)] = a as i8 as i32 as u32',
  'SextB => x[usize::from(insn.r1)] = a & 0xff'),
 ('zexth-sign',
  'ZextH => x[usize::from(insn.r1)] = a & 0xffff',
  'ZextH => x[usize::from(insn.r1)] = a as i16 as i32 as u32'),
 ('sexth-zero',
  'SextH => x[usize::from(insn.r1)] = a as i16 as i32 as u32',
  'SextH => x[usize::from(insn.r1)] = a & 0xffff'),
 ('not', 'Not => x[usize::from(insn.r1)] = !a', 'Not => x[usize::from(insn.r1)] = a'),
 ('mul', 'a.wrapping_mul(b)', 'a.wrapping_add(b)'),
 ('move-swap', 'if raw & 0x40 == 0', 'if raw & 0x40 != 0'),
 ('store-byte', '.write8(address, b as u8)', '.write8(address, a as u8)'),
 ('store-half', '.write16(address, b as u16)', '.write16(address, a as u16)'),
 ('byte-offset',
  'i.imm = u32::from(((raw >> 5) & 1) * 2 + ((raw >> 6) & 1));\n            Lbu',
  'i.imm = u32::from(((raw >> 6) & 1) * 2 + ((raw >> 5) & 1));\n            Lbu'),
 ('pop-partial',
  'x[usize::from(reg)] = bus.read32(address).map_err(fault)?;',
  'x[usize::from(reg)] = bus.read32(address).map_err(fault)?; cpu.x[usize::from(reg)] = '
  'x[usize::from(reg)];'),
 ('retire', 'cpu.retired_count += 1;', 'cpu.retired_count += 2;')]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', required=True, type=Path)
    a = ap.parse_args(); a.output.mkdir(parents=True, exist_ok=True)
    root = Path(__file__).resolve().parent.parent
    source = root/'esp32s31/src/isa_zc.rs'; original = source.read_bytes()
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('set CARGO_TARGET_DIR outside control checkout')
    command = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '-p', 'esp32s31', '--lib', 'isa_zc::tests::']
    def run(label):
        result = subprocess.run(command, cwd=root, capture_output=True, text=True)
        text = result.stdout + result.stderr
        (a.output/(label+'.log')).write_text(text)
        return result.returncode, text
    baseline, _ = run('baseline')
    if baseline:
        raise RuntimeError('baseline fails')
    results = []
    try:
        for label, before, after in MUTANTS:
            before, after = before.encode(), after.encode()
            if original.count(before) != 1:
                raise RuntimeError('mutation anchor not unique: '+label)
            source.write_bytes(original.replace(before, after))
            code, text = run(label)
            failed = [line.split(' ... ')[0].removeprefix('test ') for line in text.splitlines()
                      if line.startswith('test isa_zc::tests::') and line.endswith(' ... FAILED')]
            killed = code != 0 and bool(failed) and 'assertion' in text and 'could not compile' not in text
            results.append(dict(mutation=label, exit=code, killed=killed, tests=failed))
    finally:
        source.write_bytes(original)
        assert source.read_bytes() == original
    restored, _ = run('restored')
    payload = dict(source_sha256=hashlib.sha256(original).hexdigest(), command=command,
                   baseline_exit=baseline, restored_exit=restored, results=results)
    (a.output/'mutations.json').write_text(json.dumps(payload, indent=2)+'\n')
    print(json.dumps(payload, sort_keys=True))
    if restored or not all(r['killed'] for r in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()

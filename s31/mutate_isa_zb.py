#!/usr/bin/env python3
"""Serial assertion-killed ISA mutations; restore exact source bytes in finally."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

MUTANTS = [
    ('sh1add', '(a << 1).wrapping_add(b)', '(a << 2).wrapping_add(b)'),
    ('sh2add', '(a << 2).wrapping_add(b)', '(b << 2).wrapping_add(a)'),
    ('sh3add', '(a << 3).wrapping_add(b)', '(a << 2).wrapping_add(b)'),
    ('andn', 'Andn => a & !b', 'Andn => b & !a'),
    ('orn', 'Orn => a | !b', 'Orn => b | !a'),
    ('maxu', 'Maxu => a.max(b)', 'Maxu => (a as i32).max(b as i32) as u32'),
    ('minu', 'Minu => a.min(b)', 'Minu => (a as i32).min(b as i32) as u32'),
    ('ctz', 'Ctz => a.trailing_zeros()', 'Ctz => a.leading_zeros()'),
    ('cpop', 'Cpop => a.count_ones()', 'Cpop => a.count_zeros()'),
    ('orc_b', 'if v == 0 { 0 } else { 255 }', 'if v == 0 { 255 } else { 0 }'),
    ('rev8', 'Rev8 => a.swap_bytes()', 'Rev8 => a'),
    ('rori', 'Rori => a.rotate_right(imm)', 'Rori => a.rotate_left(imm)'),
    ('zext_h', 'ZextH => a & 0xffff', 'ZextH => a as i16 as i32 as u32'),
    ('bclr', 'Bclr => a & !(1 << sh)', 'Bclr => a | (1 << sh)'),
    ('bclri', 'Bclri => a & !(1 << imm)', 'Bclri => a | (1 << imm)'),
    ('bext', 'Bext => (a >> sh) & 1', 'Bext => (a >> sh) & 3'),
    ('bexti', 'Bexti => (a >> imm) & 1', 'Bexti => (a >> imm) & 3'),
    ('binvi', 'Binvi => a ^ (1 << imm)', 'Binvi => a | (1 << imm)'),
    ('bset', 'Bset => a | (1 << sh)', 'Bset => a ^ (1 << sh)'),
    ('bseti', 'Bseti => a | (1 << imm)', 'Bseti => a ^ (1 << imm)'),
    ('wrong-funct', '(0x33, 2, 0x10) => Sh1add', '(0x33, 2, 0x10) => Sh2add'),
    ('retire', 'cpu.retired_count += 1;', 'cpu.retired_count += 2;'),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', required=True, type=Path)
    a = ap.parse_args(); a.output.mkdir(parents=True, exist_ok=True)
    root = Path(__file__).resolve().parent.parent
    source = root/'esp32s31/src/isa_zb.rs'; original = source.read_bytes()
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('set CARGO_TARGET_DIR outside control checkout')
    command = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '-p', 'esp32s31', '--lib', 'isa_zb::tests::']
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
                      if line.startswith('test isa_zb::tests::') and line.endswith(' ... FAILED')]
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

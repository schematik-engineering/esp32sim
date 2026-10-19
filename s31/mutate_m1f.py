#!/usr/bin/env python3
"""Assertion-killed mutations of M1f controls, restoring exact source."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

MHCR_TEST = 'mhcr::tests::mhcr_mapping_retention_all_forms_and_counters'
REG_TEST = 'boot_regs::tests::boot_regs_mapping_reset_values_and_retention'
MUTANTS = []
for label, before, after in [
    ('reset', '#[derive(Default)]', 'impl Default for Mhcr { fn default() -> Self { Self { value: 1 } } }'),
    ('map', '!= 0x7c1', '!= 0x7c2'),
    ('old', 'let old = self.value;', 'let old = 0;'),
    ('exchange', '_ => source,', '_ => 0,'),
    ('set', 'old | source', 'old & source'),
    ('clear', 'old & !source', 'old | source'),
    ('immediate', 'insn.rs1 as u32\n', '0\n'),
    ('readback', 'cpu.x[insn.rd as usize] = old;', 'cpu.x[insn.rd as usize] = 0;'),
    ('pc', 'wrapping_add(4)', 'wrapping_add(2)'),
    ('retired', 'cpu.retired_count += 1;', 'cpu.retired_count += 0;'),
    ('insn', 'cpu.insn_count += 1;', 'cpu.insn_count += 0;'),
    ('cycle', 'cpu.cycle_count += 1;', 'cpu.cycle_count += 0;'),
]:
    MUTANTS.append(('mhcr-' + label, 'mhcr.rs', before, after, MHCR_TEST))
MUTANTS.append(('mhcr-wiring', 'lib.rs', 'if self.mhcr.execute(&insn, &mut self.cpu) {',
                'if false && self.mhcr.execute(&insn, &mut self.cpu) {', MHCR_TEST))
for label, before, after in [
    ('date-map', '0x2081_40fc', '0x2081_40f8'),
    ('rng-map', '0x2071_003c', '0x2071_0038'),
    ('conf-map', '0x2081_4000', '0x2081_4004'),
    ('date-reset', 'date: 2426672', 'date: 0'),
    ('rng-reset', 'rng: 0x4000_0000', 'rng: 0'),
    ('conf-reset', 'conf: 1 << 30', 'conf: 0'),
    ('date-value', 'value & 0x1fff_ffff', 'value & 0x0fff_ffff'),
    ('rng-value', 'value & 0xc000_0000', 'value & 0x4000_0000'),
    ('conf-value', 'value & !(1 << 28)', 'value'),
    ('rng-reset-conf', 'self.conf = 1 << 30;', 'self.conf = 0;'),
    ('held-reset', 'if self.rng & (1 << 31) == 0 {', 'if true {'),
]:
    MUTANTS.append((label, 'boot_regs.rs', before, after, REG_TEST))

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', required=True, type=Path)
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    assert os.environ.get('CARGO_TARGET_DIR'), 'set CARGO_TARGET_DIR'
    root = Path(__file__).resolve().parent.parent
    originals = {f: (root/'esp32s31/src'/f).read_bytes() for f in {m[1] for m in MUTANTS}}
    command = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '--release', '-p', 'esp32s31', '--lib']
    def run(label, test=None):
        cmd = command + ([test, '--', '--exact'] if test else [])
        proc = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
        output = proc.stdout + proc.stderr
        (args.output/(label+'.log')).write_text(output)
        return proc.returncode, output
    baseline, _ = run('baseline')
    assert baseline == 0, 'baseline failed'
    results = []
    try:
        for label, filename, before, after, test in MUTANTS:
            original = originals[filename].decode()
            assert original.count(before) == 1, label
            path = root/'esp32s31/src'/filename
            path.write_text(original.replace(before, after))
            code, output = run(label, test)
            killed = code == 101 and (test+' ... FAILED') in output and 'assertion' in output
            results.append(dict(mutation=label, test=test, exit=code, assertion_killed=killed))
            path.write_bytes(originals[filename])
    finally:
        for filename, original in originals.items():
            (root/'esp32s31/src'/filename).write_bytes(original)
    restored, _ = run('restored')
    payload = dict(source_sha256={f:hashlib.sha256(b).hexdigest() for f,b in originals.items()},
                   baseline_exit=baseline, restored_exit=restored, results=results)
    (args.output/'mutations.json').write_text(json.dumps(payload, indent=2)+'\n')
    print(json.dumps(payload, sort_keys=True))
    if restored or not all(r['assertion_killed'] for r in results):
        raise SystemExit(1)

if __name__ == '__main__':
    main()

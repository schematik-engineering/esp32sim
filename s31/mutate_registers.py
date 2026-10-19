#!/usr/bin/env python3
"""Serial assertion mutations for M1b; restore exact source even on failure."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

MUTANTS = [
    ('pma-address-map', 'if insn.imm as u32 == 0xbdc {', 'if insn.imm as u32 == 0xbdd {', 'pma_addr12_first_no_read_write_and_config_stop'),
    ('pma-value-guard', '|| value != 0x0b80_0fff', '|| false', 'pma_unknown_reads_values_operations_and_rewrites_stop_atomically'),
    ('pma-value-store', 'self.pma_addr12 = Some(value);', 'self.pma_addr12 = Some(0);', 'pma_addr12_first_no_read_write_and_config_stop'),
    ('pma-no-read', '|| insn.rd != 0', '|| false', 'pma_unknown_reads_values_operations_and_rewrites_stop_atomically'),
    ('pma-op', 'if insn.op != Op::Csrrw', 'if false', 'pma_unknown_reads_values_operations_and_rewrites_stop_atomically'),
    ('pma-rewrite', '|| self.pma_addr12.is_some()', '|| false', 'pma_unknown_reads_values_operations_and_rewrites_stop_atomically'),
    ('pma-zero-source', 'let value = if insn.rs1 == 0 {', 'let value = if false {', 'pma_unknown_reads_values_operations_and_rewrites_stop_atomically'),
    ('pma-pc', 'self.cpu.pc = pc.wrapping_add(4);', 'self.cpu.pc = pc.wrapping_add(2);', 'pma_addr12_first_no_read_write_and_config_stop'),
    ('pma-retired', 'self.cpu.retired_count += 1;', 'self.cpu.retired_count += 0;', 'pma_addr12_first_no_read_write_and_config_stop'),
    ('pma-insns', 'self.cpu.insn_count += 1;', 'self.cpu.insn_count += 0;', 'pma_addr12_first_no_read_write_and_config_stop'),
    ('pma-cycles', 'self.cpu.cycle_count += 1;', 'self.cpu.cycle_count += 0;', 'pma_addr12_first_no_read_write_and_config_stop'),
    ('reset-cause-map', 'HPCORE0_RESET_CAUSE: u32 = 0x2070_1030', 'HPCORE0_RESET_CAUSE: u32 = 0x2070_1034', 'reset_cause_mapping_and_write_triggers'),
    ('clic-config-map', 'CLIC_CONFIG: u32 = 0x1080_0000', 'CLIC_CONFIG: u32 = 0x1080_0004', 'clic_config_mapping_and_fields'),
    ('power-on-cause', 'POWER_ON_RESET: u32 = 1 << 1', 'POWER_ON_RESET: u32 = 0', 'reset_cause_mapping_and_write_triggers'),
    ('reset-cause-clear', 'self.reset_cause &= !0x7e;', 'self.reset_cause &= !0;', 'reset_cause_mapping_and_write_triggers'),
    ('reset-flag-clear', 'self.reset_cause &= !1;', 'self.reset_cause &= !0;', 'reset_cause_mapping_and_write_triggers'),
    ('clic-ro-bit', 'self.clic_config = (v & 0x1e) | 1;', 'self.clic_config = v & 0x1e;', 'clic_config_mapping_and_fields'),
    ('clic-level-mask', 'self.clic_config = (v & 0x1e) | 1;', 'self.clic_config = (v & 0x0e) | 1;', 'clic_config_mapping_and_fields'),
    ('counter-mask', '*v &= 7;', '*v &= u32::MAX;', 'reset_prefix_standard_csr_instructions'),
    ('unknown-csr', '0x300 | 0x305 | 0x306 | 0xf14', '0x300 | 0x305 | 0x306 | 0x307 | 0xf14', 'unknown_csr_does_not_use_generic_readback'),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', required=True, type=Path)
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('set CARGO_TARGET_DIR outside the control checkout')
    root = Path(__file__).resolve().parent.parent
    source = root / 'esp32s31/src/lib.rs'
    original = source.read_bytes()
    text = original.decode()
    command = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '--release', '-p', 'esp32s31', '--lib']

    def run(label, test=None):
        cmd = command + (['tests::' + test, '--', '--exact'] if test else [])
        proc = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
        output = proc.stdout + proc.stderr
        (args.output / (label + '.log')).write_text(output)
        return proc.returncode, output

    baseline, _ = run('baseline')
    if baseline:
        raise RuntimeError('baseline fails')
    results = []
    try:
        for label, before, after, test in MUTANTS:
            if text.count(before) != 1:
                raise RuntimeError('non-unique mutation anchor: ' + label)
            source.write_text(text.replace(before, after))
            code, output = run(label, test)
            killed = code == 101 and ('tests::' + test + ' ... FAILED') in output and 'assertion' in output
            results.append(dict(mutation=label, test=test, exit=code, assertion_killed=killed))
    finally:
        source.write_bytes(original)
    restored, _ = run('restored')
    payload = dict(source_sha256=hashlib.sha256(original).hexdigest(), command=command,
                   baseline_exit=baseline, restored_exit=restored, results=results)
    (args.output / 'mutations.json').write_text(json.dumps(payload, indent=2) + '\n')
    print(json.dumps(payload, sort_keys=True))
    if restored or not all(r['assertion_killed'] for r in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()

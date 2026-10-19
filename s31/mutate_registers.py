#!/usr/bin/env python3
"""Serial assertion mutations for M1b; restore exact source even on failure."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

MUTANTS = [
    ('pma-range-low', '(0xbc0..=0xbdf)', '(0xbc1..=0xbdf)', 'pma_all_entries_mapping_values_and_counters'),
    ('pma-range-high', '(0xbc0..=0xbdf)', '(0xbc0..=0xbde)', 'pma_all_entries_mapping_values_and_counters'),
    ('pma-no-reset', 'pma: [None; 32]', 'pma: [Some(0); 32]', 'pma_unknown_reads_stop_atomically'),
    ('pma-no-read', 'let reads = !exchange || insn.rd != 0;', 'let reads = false;', 'pma_unknown_reads_stop_atomically'),
    ('pma-reads-rd0', 'let reads = !exchange || insn.rd != 0;', 'let reads = insn.rd != 0;', 'pma_unknown_reads_stop_atomically'),
    ('pma-zero-source', '} else if insn.rs1 == 0 {', '} else if false {', 'pma_zicsr_operations_aliases_and_zero'),
    ('pma-immediate', 'insn.rs1 as u32\n', 'self.cpu.x[insn.rs1 as usize]\n', 'pma_zicsr_operations_aliases_and_zero'),
    ('pma-set', 'old.unwrap() | value', 'old.unwrap() & value', 'pma_zicsr_operations_aliases_and_zero'),
    ('pma-clear', 'old.unwrap() & !value', 'old.unwrap() | value', 'pma_zicsr_operations_aliases_and_zero'),
    ('pma-readback', 'self.cpu.x[insn.rd as usize] = old.unwrap();', 'self.cpu.x[insn.rd as usize] = 0;', 'pma_zicsr_operations_aliases_and_zero'),
    ('pma-lock', '(writes && locked)', '(false && locked)', 'pma_locks_guard_cfg_and_address_without_mutation'),
    ('pma-lock-bit', 'cfg & (1 << 29)', 'cfg & (1 << 28)', 'pma_locks_guard_cfg_and_address_without_mutation'),
    ('pma-lock-pair', 'self.pma[index % 16]', 'self.pma[index]', 'pma_locks_guard_cfg_and_address_without_mutation'),
    ('pma-write-source-index', 'let writes = exchange || insn.rs1 != 0;', 'let writes = exchange || value != 0;', 'pma_locks_guard_cfg_and_address_without_mutation'),
    ('pma-readonly', 'let writes = exchange || insn.rs1 != 0;', 'let writes = true;', 'pma_locks_guard_cfg_and_address_without_mutation'),
    ('pma-pc', 'self.cpu.pc = pc.wrapping_add(4);', 'self.cpu.pc = pc.wrapping_add(2);', 'pma_all_entries_mapping_values_and_counters'),
    ('pma-retired', 'self.cpu.retired_count += 1;', 'self.cpu.retired_count += 0;', 'pma_all_entries_mapping_values_and_counters'),
    ('pma-insns', 'self.cpu.insn_count += 1;', 'self.cpu.insn_count += 0;', 'pma_all_entries_mapping_values_and_counters'),
    ('pma-cycles', 'self.cpu.cycle_count += 1;', 'self.cpu.cycle_count += 0;', 'pma_all_entries_mapping_values_and_counters'),
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
# One mapping and one retained-value mutant for EACH of the 32 CSR addresses.
for csr in range(0xbc0, 0xbe0):
    MUTANTS += [
        (f'pma-map-{csr:03x}', 'if (0xbc0..=0xbdf).contains(&csr) {\n                    let index = (csr - 0xbc0) as usize;',
         f'if (0xbc0..=0xbdf).contains(&csr) {{\n                    let index = ((csr - 0xbc0) as usize + usize::from(csr == 0x{csr:x})) % 32;',
         'pma_all_entries_mapping_values_and_counters'),
        (f'pma-value-{csr:03x}', 'self.pma[index] = Some(new);',
         f'self.pma[index] = Some(new ^ u32::from(csr == 0x{csr:x}));',
         'pma_all_entries_mapping_values_and_counters'),
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

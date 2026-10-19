#!/usr/bin/env python3
"""Run serially, with no other cargo checks in this worktree; restore in finally."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

MUTANTS = [
    ('hp-sram-base', '0x2f00_0000, 0x2f08_0000', '0x2f01_0000, 0x2f08_0000'),
    ('rom-upper-bound', '(RESET, 0x2f85_0000)', '(RESET, 0x2f84_0000)'),
    ('flash-nc-offset', 'FLASH_NC_OFFSET: u32 = 0x6000_0000', 'FLASH_NC_OFFSET: u32 = 0x5000_0000'),
    ('psram-nc-offset', 'PSRAM_NC_OFFSET: u32 = 0x7000_0000', 'PSRAM_NC_OFFSET: u32 = 0x6000_0000'),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', required=True, type=Path)
    a = ap.parse_args(); a.output.mkdir(parents=True, exist_ok=True)
    root = Path(__file__).resolve().parent.parent
    source = root / 'esp32s31/src/lib.rs'; original = source.read_text()
    if not os.environ.get('CARGO_TARGET_DIR'): raise RuntimeError('set CARGO_TARGET_DIR outside control checkout')
    command = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '-p', 'esp32s31', '--lib', 'tests::map_pins_every_aperture_and_boundary', '--', '--exact']
    def run(label):
        result = subprocess.run(command, cwd=root, capture_output=True, text=True)
        text = result.stdout + result.stderr
        (a.output / (label+'.log')).write_text(text)
        return result.returncode, text
    baseline, _ = run('baseline')
    if baseline: raise RuntimeError('baseline test fails')
    results = []
    try:
        for label, before, after in MUTANTS:
            if original.count(before) != 1: raise RuntimeError('mutation anchor not unique: '+label)
            source.write_text(original.replace(before, after))
            code, text = run(label)
            killed = code != 0 and 'tests::map_pins_every_aperture_and_boundary ... FAILED' in text and 'assertion' in text
            results.append(dict(mutation=label, exit=code, killed=killed, test='tests::map_pins_every_aperture_and_boundary'))
    finally:
        source.write_text(original)
    restored, _ = run('restored')
    payload = dict(source_sha256=hashlib.sha256(original.encode()).hexdigest(), command=command, baseline_exit=baseline, restored_exit=restored, results=results)
    (a.output/'mutations.json').write_text(json.dumps(payload, indent=2)+'\n')
    print(json.dumps(payload, sort_keys=True))
    if restored or not all(r['killed'] for r in results): raise SystemExit(1)


if __name__ == '__main__': main()

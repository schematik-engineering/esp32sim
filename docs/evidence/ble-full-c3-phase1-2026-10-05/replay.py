#!/usr/bin/env python3
"""Run the unchanged Server with the modeled C3 controller; no guest pokes or hooks."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser(description=__doc__)
for name in ('emulator', 'build', 'rom', 'output'):
    p.add_argument('--' + name, type=Path, required=True)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
cmd = [str(a.emulator.resolve()), '--boot', 'rom', '--rom', str(a.rom), '--flash-mb', '4']
inputs = {'rom': a.rom}
for flag, suffix in [('bootloader', 'bootloader.bin'), ('ptable', 'partitions.bin'), ('app', 'bin'), ('elf', 'elf')]:
    path = a.build / ('Server.ino.' + suffix)
    inputs[flag] = path
    cmd += ['--' + flag, str(path)]
cmd += ['--ble', 'full', '--max-seconds', '1', '--no-dump', '--irq-latency', '--debug', 'mmio']
for prefix in ['r_sch', 'r_lld_adv', 'r_rwble_isr']:
    cmd += ['--trace-fn', prefix]
# Addresses observed for this pinned specimen, not an emulator memory-layout contract.
for addr, words in [(0x60031204, 56), (0x60031100, 1), (0x3fca7794, 8), (0x3fca78f8, 24),
                    (0x3fca80dc, 16), (0x3fca85e0, 16), (0x3fc9c680, 12)]:
    cmd += ['--peek', f'{addr:#x},{words}']
r = subprocess.run(cmd, capture_output=True, check=True)
stdout, stderr = r.stdout.decode(), r.stderr.decode()
assert 'Characteristic defined! Now you can read it in your phone!' in stdout
for expected in ['r_sch_prog_ble_push_hack(a0=', 'r_lld_adv_evt_start_cbk(a0=', 'sources [8]',
                 '60031100: 80000000', '0 exceptions', '160000000 insns']:
    assert expected in stderr, expected
assert 'assert' not in stdout
sha = lambda b: hashlib.sha256(b).hexdigest()
captures = {}
for name, data in [('stdout', stdout), ('stderr', stderr)]:
    original = data.encode()
    for path, label in [(str(a.build.resolve()), '<BUILD>'), (str(a.rom.resolve()), '<ROM>'),
                        (str(Path.cwd()), '<WORKTREE>'), (str(Path.home()), '<HOME>')]:
        data = data.replace(path, label)
    encoded = data.encode()
    (a.output / ('server.' + name)).write_bytes(encoded)
    captures[name] = {'original_sha256': sha(original), 'sanitized_sha256': sha(encoded), 'bytes': len(encoded)}
summary = {'inputs': {k: {'sha256': sha(v.read_bytes()), 'bytes': v.stat().st_size} for k, v in inputs.items()},
           'captures': captures, 'checks': 'pass',
           'functions': [line for line in stderr.splitlines() if line.startswith('[fn]')],
           'snapshots_after_run': re.findall(r'\[peek after run\]\n((?:[0-9a-f]{8}: [0-9a-f]{8}\n)+)', stderr),
           'stop': next(line for line in stderr.splitlines() if line.startswith('[emu] stop:')),
           'source_8': [line.strip() for line in stderr.splitlines() if 'sources [8]' in line]}
(a.output / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
# Keep ordered first/last values and counts rather than a raw MMIO capture in Git.
registers = {}
for line in stderr.splitlines():
    if line.startswith('[emu] stop:'):
        break
    m = re.match(r'\[(rd|wr)\] .*\((0x600(?:31|11)[0-9a-f]{3})\) (?:->|<-) (0x[0-9a-f]+) pc=(0x[0-9a-f]+)', line)
    if not m:
        continue
    direction, addr, value, pc = m.groups()
    if pc == "0x00000000":
        continue
    key = (addr, direction, pc)
    if key not in registers:
        registers[key] = [value, value, 0]
    registers[key][1] = value
    registers[key][2] += 1
with (a.output / 'registers.tsv').open('w') as f:
    f.write('address\tdirection\tpc\tfirst\tlast\tcount\n')
    for key, values in registers.items():
        f.write('\t'.join(map(str, (*key, *values))) + '\n')
print('Server initialized, source 8 delivered, first event programmed; no radio transmission modeled.')

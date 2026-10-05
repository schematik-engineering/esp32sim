#!/usr/bin/env python3
"""Replay unchanged C3 Server; keep raw captures outside Git and write a compact receipt."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess

p = argparse.ArgumentParser(description=__doc__)
for name in ('emulator', 'build', 'rom', 'output'):
    p.add_argument('--' + name, type=Path, required=True)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
cmd = [str(a.emulator.resolve()), '--boot', 'rom', '--rom', str(a.rom), '--flash-mb', '4',
       '--ble', 'full', '--ble-observe', '--max-seconds', '2', '--no-dump', '--irq-latency']
inputs = {'rom': a.rom}
for flag, suffix in [('bootloader', 'bootloader.bin'), ('ptable', 'partitions.bin'), ('app', 'bin'), ('elf', 'elf')]:
    path = a.build / ('Server.ino.' + suffix)
    inputs[flag] = path
    cmd += ['--' + flag, str(path)]
for prefix in ['r_sch_prog_end_isr_handler', 'r_lld_adv_frm_isr', 'r_sch_prog_ble_push_hack']:
    cmd += ['--trace-fn', prefix]
# Pinned-build pointers, only for evidence; none are hard-coded into the model.
for addr, count in [(0x3fcdfcc4, 1), (0x3fca2e3c, 1), (0x3fca1944, 6), (0x60031204, 9)]:
    cmd += ['--peek', f'{addr:#x},{count}']
r = subprocess.run(cmd, capture_output=True, check=True)
stdout, stderr = r.stdout.decode(), r.stderr.decode()
assert 'Characteristic defined! Now you can read it in your phone!' in stdout
assert 'assert' not in stdout and '[ble-error]' not in stderr
assert '0 exceptions' in stderr
assert '3fca2e3c: 00000060' in stderr
packets = [line for line in stderr.splitlines() if line.startswith('[ble-air]')]
assert len(packets) >= 15 and len(packets) % 3 == 0
for i, line in enumerate(packets):
    assert f'channel={37 + i % 3} type=ADV_IND AdvA=60:55:f9:00:11:24' in line
    assert 'service=4fafc201-1fb5-459e-8fcc-c5c9c331914b' in line
starts = [int(re.search(r'hus=(\d+)', line)[1]) for line in packets[::3]]
intervals = [(b - a) / 2 for a, b in zip(starts, starts[1:])]
assert all(60000 <= v <= 70000 for v in intervals)
configured = next(line for line in stderr.splitlines() if line.startswith('[ble-config]'))
assert 'SCAN_RSP name="BLE Server Example"' in configured
sha = lambda data: hashlib.sha256(data).hexdigest()
captures = {}
for name, data in [('stdout', stdout), ('stderr', stderr)]:
    original = data.encode()
    for path, label in [(str(a.build.resolve()), '<BUILD>'), (str(a.rom.resolve()), '<ROM>'),
                        (str(Path.cwd()), '<WORKTREE>'), (str(Path.home()), '<HOME>')]:
        data = data.replace(path, label)
    encoded = data.encode()
    (a.output / ('server.' + name)).write_bytes(encoded)
    captures[name] = {'original_sha256': sha(original), 'sanitized_sha256': sha(encoded), 'bytes': len(encoded)}
summary = {'checks': 'pass', 'modeled_seconds': 2, 'events': len(starts), 'packets': len(packets),
           'first_five_event_hus': starts[:5], 'interval_us': {'min': min(intervals), 'max': max(intervals), 'median': statistics.median(intervals)},
           'configured_interval_units_625us': 96, 'sample': packets[0], 'configured': configured,
           'functions': {name: sum(name + '(a0=' in line for line in stderr.splitlines()) for name in
                         ['r_sch_prog_end_isr_handler', 'r_lld_adv_frm_isr_eco', 'r_sch_prog_ble_push_hack']},
           'stop': next(line for line in stderr.splitlines() if line.startswith('[emu] stop:')),
           'source_8': [line.strip() for line in stderr.splitlines() if 'sources [8]' in line],
           'inputs': {k: {'sha256': sha(v.read_bytes()), 'bytes': v.stat().st_size} for k, v in inputs.items()},
           'binary_sha256': sha(a.emulator.read_bytes()), 'captures': captures}
(a.output / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps({k: summary[k] for k in ('events', 'packets', 'interval_us', 'checks')}))

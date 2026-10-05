#!/usr/bin/env python3
"""Capture a final active-scan run; require caller-owned build, ROM and output paths."""
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
cmd = [str(a.emulator.resolve()), '--boot', 'rom', '--rom', str(a.rom),
       '--flash-mb', '4', '--ble', 'full', '--ble-observe', '--ble-scan',
       '--max-seconds', '2', '--no-dump', '--trace-fn', 'r_lld_rxdesc_free']
inputs = {'rom': a.rom}
for flag, suffix in [('bootloader', 'bootloader.bin'), ('ptable', 'partitions.bin'),
                     ('app', 'bin'), ('elf', 'elf')]:
    path = a.build / ('Server.ino.' + suffix)
    inputs[flag] = path
    cmd += ['--' + flag, str(path)]
r = subprocess.run(cmd, capture_output=True, check=True)
stdout, stderr = r.stdout.decode(), r.stderr.decode()
assert 'Characteristic defined! Now you can read it in your phone!' in stdout
assert 'assert' not in stdout and '[ble-error]' not in stderr
assert '0 exceptions' in stderr
adv = [s for s in stderr.splitlines() if s.startswith('[ble-air]') and 'type=ADV_IND' in s]
rsp = [s for s in stderr.splitlines() if s.startswith('[ble-air]') and 'type=SCAN_RSP' in s]
req = [s for s in stderr.splitlines() if s.startswith('[ble-central]')]
assert len(adv) >= 15 and len(adv) == len(rsp) == len(req)
time = lambda s: int(re.search(r'hus=(\d+)', s)[1])
for i, (v, q, s) in enumerate(zip(adv, req, rsp)):
    assert all(f'channel={37 + i % 3}' in line for line in (v, q, s))
    assert 'name="BLE Server Example"' in s
    assert time(q) - time(v) == 988
    assert time(s) - time(q) == 652
starts = list(map(time, adv[::3]))
intervals = [(b - a) / 2 for a, b in zip(starts, starts[1:])]
assert all(60000 <= v <= 70000 for v in intervals)
frees = stderr.count('r_lld_rxdesc_free(a0=')
assert frees >= len(req)
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
result = {
    'checks': 'pass', 'modeled_seconds': 2, 'events': len(starts),
    'advertisements': len(adv), 'scan_requests': len(req), 'scan_responses': len(rsp),
    'rx_free_trace_entries': frees, 'trace_note': 'ROM veneer and body can both be counted',
    'first_five_event_hus': starts[:5],
    'interval_us': {'min': min(intervals), 'max': max(intervals), 'median': statistics.median(intervals)},
    'scan_req_ifs_us': 150, 'scan_rsp_ifs_us': 150, 'sample': rsp[0],
    'stop': next(s for s in stderr.splitlines() if s.startswith('[emu] stop:')),
    'inputs': {k: {'sha256': sha(v.read_bytes()), 'bytes': v.stat().st_size} for k, v in inputs.items()},
    'binary_sha256': sha(a.emulator.read_bytes()), 'captures': captures,
}
(a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k: result[k] for k in ('events', 'scan_responses', 'checks')}))

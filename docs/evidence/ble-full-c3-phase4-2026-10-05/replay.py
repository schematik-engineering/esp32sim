#!/usr/bin/env python3
"""Capture a final connection/timeout run; require caller-owned build, ROM and output paths."""
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
       '--flash-mb', '4', '--ble', 'full', '--ble-observe', '--ble-connect', '--ble-stop-after-ms', '2500',
       '--max-seconds', '6', '--no-dump', '--trace-fn', 'r_llc_disconnect_end']
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
lines = stderr.splitlines()
events = [s for s in lines if s.startswith('[ble-connection]')]
data = [s for s in lines if 'type=DATA' in s]
time = lambda s: int(re.search(r'hus=(\d+)', s)[1])
anchor = lambda s: int(re.search(r'anchor_hus=(\d+)', s)[1])
connect = next(s for s in lines if 'type=CONNECT_IND' in s)
disconnect = next(s for s in lines if 'r_llc_disconnect_end(a0=0x1' in s and 'a2=0x8' in s)
resumed = next(s for s in lines if 'disconnected advertising_resumed' in s)
adv_after = [s for s in lines[lines.index(resumed):] if 'type=ADV_IND' in s]
assert anchor(events[0]) - time(connect) == 18204
assert len(data) % 2 == 0 and len(data) >= 140 and adv_after
for i, event in enumerate(events):
    assert anchor(event) == anchor(events[0]) + i * 60000
    assert int(re.search(r'channel=(\d+)', event)[1]) == (i + 1) * 5 % 37
for i in range(0, len(data), 2):
    assert time(data[i + 1]) - time(data[i]) == 460
assert time(data[-2]) - time(data[0]) >= 4000000
sha = lambda b: hashlib.sha256(b).hexdigest()
captures = {}
for name, capture in [('stdout', stdout), ('stderr', stderr)]:
    original = capture.encode()
    for path, label in [(str(a.build.resolve()), '<BUILD>'), (str(a.rom.resolve()), '<ROM>'),
                        (str(Path.cwd()), '<WORKTREE>'), (str(Path.home()), '<HOME>')]:
        capture = capture.replace(path, label)
    encoded = capture.encode()
    (a.output / ('server.' + name)).write_bytes(encoded)
    captures[name] = {'original_sha256': sha(original), 'sanitized_sha256': sha(encoded), 'bytes': len(encoded)}
result = {
    'checks': 'pass', 'modeled_seconds': 6, 'connection_events': len(events),
    'central_pdus': len(data) // 2, 'peripheral_pdus': len(data) // 2,
    'exchange_duration_us': (time(data[-2]) - time(data[0])) / 2,
    'anchor_interval_us': 30000, 'ifs_us': 150,
    'connect': connect, 'first_event': events[0], 'last_event': events[-1],
    'first_exchange': data[:4], 'disconnect': disconnect,
    'last_central_hus': time(data[-2]), 'resumed': resumed,
    'resumed_advertisements': len(adv_after),
    'resumed_advertisement_sample': adv_after[0],
    'guest_errors': [s for s in stdout.splitlines() if s.startswith('E (')],
    'stop': next(s for s in lines if s.startswith('[emu] stop:')),
    'inputs': {k: {'sha256': sha(v.read_bytes()), 'bytes': v.stat().st_size} for k, v in inputs.items()},
    'binary_sha256': sha(a.emulator.read_bytes()), 'captures': captures,
}
(a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k: result[k] for k in ('connection_events', 'exchange_duration_us', 'checks')}))

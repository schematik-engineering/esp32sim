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
       '--flash-mb', '4', '--ble', 'full', '--ble-observe', '--ble-connect', '--ble-read-uuid', '4fafc201-1fb5-459e-8fcc-c5c9c331914b', 'beb5483e-36e1-4688-b7f5-ea07361b26a8',
       '--max-seconds', '3', '--no-dump', '--trace-fn', 'r_lld_con_tx_isr']
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
value = next(s for s in lines if s.startswith('[ble-att] value='))
assert 'text="Hello World says Neil"' in value
assert '05000400070e001000' in stderr
assert '1700040009150f000a1000a8261b3607eaf5b78846e1363e48b5be' in stderr
assert 'r_lld_con_tx_isr(a0=0x1' in stderr
assert '[ble-state] disconnected' not in stderr
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
    'checks': 'pass', 'modeled_seconds': 3, 'connection_events': len(events),
    'att_read': value, 'first_event': events[0], 'last_event': events[-1],
    'protocol_sample': data[:20],
    'stop': next(s for s in lines if s.startswith('[emu] stop:')),
    'inputs': {k: {'sha256': sha(v.read_bytes()), 'bytes': v.stat().st_size} for k, v in inputs.items()},
    'binary_sha256': sha(a.emulator.read_bytes()), 'captures': captures,
}
(a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k: result[k] for k in ('connection_events', 'att_read', 'checks')}))

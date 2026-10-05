#!/usr/bin/env python3
"""Replay diagnostic register pokes. This is not a BLE hardware model."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def script(stage):
    if stage == 0:
        return ''
    events = []
    for tick in range(301, 10000):
        time = tick / 10000
        events.extend([(time, '60031020', 624), (time, '6003101c', int(time * 3200))])
    for minimum, time, addr, value in [
        (2, .029, '60031004', 0x09001b00),
        (3, .052, '60031000', 0x0010070f),
        (4, .054, '60031000', 0x0010060f),
        (5, .055, '60031000', 0x0010070f),
    ]:
        if stage >= minimum:
            events.append((time, addr, value))
    return ''.join(f'{time:.4f} poke {addr} {value:08x}\n'
                   for time, addr, value in sorted(events, key=lambda e: e[0]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('emulator', 'build', 'rom', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    base = [str(args.emulator.resolve()), '--boot', 'rom', '--rom', str(args.rom),
            '--flash-mb', '4']
    for flag, suffix in [('bootloader', 'bootloader.bin'), ('ptable', 'partitions.bin'),
                         ('app', 'bin'), ('elf', 'elf')]:
        base += ['--' + flag, str(args.build / ('Server.ino.' + suffix))]
    base += ['--max-seconds', '1', '--no-dump', '--log-periph', '--profile',
             '--trace-fn', 'esp_bt_controller', '--trace-fn', 'btdm_controller',
             '--trace-fn', 'r_lld_adv']
    expected = ['r_rwip_time_get+0x1c', 'r_assert_param+0x34',
                'r_rwip_driver_init+0x10e', 'r_rwble_hw_disable+0x1c',
                'r_rwip_driver_init+0x10e', 'esp_cpu_wait_for_intr+0x18']
    results = []
    for stage, symbol in enumerate(expected):
        cmd = base.copy()
        if stage:
            path = args.output / f'stage-{stage}.script'
            path.write_text(script(stage))
            cmd += ['--script', str(path)]
        result = subprocess.run(cmd, capture_output=True, check=True)
        for name, data in [('stdout', result.stdout), ('stderr', result.stderr)]:
            (args.output / f'stage-{stage}.{name}').write_bytes(data)
        assert b'160000000 insns' in result.stderr
        assert symbol.encode() in result.stderr, (stage, symbol)
        assert (b'Characteristic defined!' in result.stdout) == (stage == 5)
        if stage == 1:
            assert b'BLE assert lld.c 324, param 00000000 09001b00' in result.stdout
        results.append({'stage': stage, 'expected_profile_symbol': symbol,
                        'check': 'pass', 'stdout_sha256': hashlib.sha256(result.stdout).hexdigest(),
                        'stderr_sha256': hashlib.sha256(result.stderr).hexdigest()})
    (args.output / 'checks.json').write_text(json.dumps(results, indent=2) + '\n')
    print('Six diagnostic stages passed; no radio PDU or milestone A claim.')


if __name__ == '__main__':
    main()

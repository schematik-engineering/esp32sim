#!/usr/bin/env python3
"""EX199 classic BLE acceptance on unchanged Arduino examples; raw logs stay outside Git."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument('--emulator', type=Path, required=True)
p.add_argument('--firmware', type=Path, required=True)
p.add_argument('--rom', type=Path, required=True)
p.add_argument('--arduino', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def clean(text):
    return text.replace(str(Path.home()), '/Users/alice')


runs = []
service = '4fafc201-1fb5-459e-8fcc-c5c9c331914b'
for example, seconds, script, required in [
    ('server', 1.3, '0.5 ble connect\n0.6 ble discover\n1.0 ble read 0x002a\n',
     ['name="BLE Server Example"', 'text="Hello World says Neil"']),
    ('notify', 2.6, '0.5 ble connect\n0.6 ble discover\n0.7 ble subscribe 0x002b\n',
     ['name="ESP32"', 'write complete handle=0x002b']),
    ('write', 1.2, '0.5 ble connect\n0.6 ble discover\n'
     '0.7 ble write 0x002a 48656c6c6f2066726f6d20686f7374\n0.8 ble read 0x002a\n',
     ['name="MyESP32"', 'New value: Hello from host', 'text="Hello from host"']),
    ('scan', 5.6, '', ['Name: esp32sim', '0000180f-0000-1000-8000-00805f9b34fb',
                      'Devices found: 1', 'Scan done!']),
    ('reboot', 1.2, '0.6 poke 3ff48000 80000000\n', ['rst:0x3 (SW_RESET)']),
]:
    source = 'server' if example == 'reboot' else example
    project = a.firmware / source
    firmware = project / '.pio/build/esp32dev'
    action = a.output / f'{example}.script'
    action.write_text(script)
    cmd = [str(a.emulator.resolve()), '--chip', 'esp32', '--boot', 'rom', '--rom', str(a.rom),
           '--flash-image', str(firmware / 'firmware.factory.bin'), '--elf', str(firmware / 'firmware.elf'),
           '--ble', '--max-seconds', str(seconds), '--no-dump', '--script', str(action)]
    if example != 'reboot':
        cmd.append('--no-reboot')
    start = time.perf_counter()
    ads = []
    with tempfile.TemporaryFile(mode='w+t') as serial_file:
        proc = subprocess.Popen(cmd, stdout=serial_file, stderr=subprocess.PIPE, text=True)
        lines = []
        for line in proc.stderr:
            lines.append(line)
            if m := re.search(r'\[ble\] t=([\d.]+)s advertising ', line):
                ads.append(dict(modeled_s=float(m[1]), wall_s=time.perf_counter() - start))
        code = proc.wait()
        wall = time.perf_counter() - start
        serial_file.seek(0)
        serial = serial_file.read()
    original = serial + ''.join(lines)
    output = clean(original)
    raw = a.output / f'{example}.log'
    raw.write_text(output)
    serial_log = a.output / f'{example}.serial.log'
    serial_log.write_text(clean(serial))
    checks = {text: text in output for text in required}
    checks['clean_stop'] = '[emu] stop: Halted' in output and all(
        text not in output for text in ['Guru Meditation', 'assert failed', 'unsupported HCI', '[ble] rejected'])
    official = a.arduino / f'libraries/BLE/examples/{source.title()}/{source.title()}.ino'
    sketch = project / 'src/main.cpp'
    checks['unchanged_official_sketch'] = sketch.read_bytes() == official.read_bytes()
    if example in ['server', 'notify', 'write']:
        checks.update({text: text in output for text in [f'uuid={service}', 'connected handle=0x0001 guest=peripheral', 'discovery complete']})
    if example in ['server', 'notify']:
        checks['advertised_service'] = f'advertising service={service}' in output
    notifications = re.findall(r'notification handle=0x002a value=([0-9a-f]+)', output)
    if example == 'notify':
        checks['four_incrementing_notifications'] = notifications == [f'{n:02x}000000' for n in range(1, 5)]
    if example == 'reboot':
        checks['advertises_before_and_after_reset'] = len(ads) == 2 and ads[0]['modeled_s'] < .6 < ads[1]['modeled_s']
        checks['reinitialized_guest'] = serial.count('Starting BLE work!') == 2
        checks['repeated_name_and_service'] = output.count(f'advertising service={service} name="BLE Server Example"') == 2
    runs.append(dict(example=example, command=[clean(x) for x in cmd], script=script,
        exit_code=code, passed=code == 0 and all(checks.values()), checks=checks,
        advertising=ads, wall_s=wall, notification_values=notifications,
        hashes={name: digest(firmware / name) for name in ['firmware.elf', 'firmware.factory.bin']},
        sketch_sha256=digest(sketch), official_sketch_sha256=digest(official),
        log_sha256=digest(raw), log_bytes=raw.stat().st_size, serial_sha256=digest(serial_log),
        original_log_sha256=hashlib.sha256(original.encode()).hexdigest(),
        stop_summary=next((line for line in output.splitlines() if 'stop:' in line), None),
        serial_output=clean(serial[serial.find('entry '):].partition('\n')[2])))
    print(example, runs[-1]['passed'], checks, flush=True)
(a.output / 'runs.json').write_text(json.dumps(dict(
    emulator_sha256=digest(a.emulator), rom_sha256=digest(a.rom),
    capture_order='stdout then stderr; advertising timed while reading stderr',
    redactions='Home paths normalized to /Users/alice home paths; BLE addresses are synthetic fixtures.',
    runs=runs), indent=2) + '\n')
raise SystemExit(0 if all(run['passed'] for run in runs) else 1)

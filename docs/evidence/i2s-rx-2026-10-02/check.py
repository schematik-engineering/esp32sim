#!/usr/bin/env python3
"""Check unchanged Arduino binaries; paths are supplied by the caller."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--emulator', type=Path, required=True)
p.add_argument('--firmware', type=Path, required=True, help='PlatformIO .pio/build directory')
p.add_argument('--rom-dir', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
results = []
for env in ['s3', 'c3', 'c6', 's3-pdm']:
    chip = env.split('-')[0]
    rom = a.rom_dir / f'esp32{chip}_rev{3 if chip == "c3" else 0}_rom.elf'
    inputs = {'rom': rom, 'bootloader': a.firmware / env / 'bootloader.bin',
              'ptable': a.firmware / env / 'partitions.bin', 'app': a.firmware / env / 'firmware.bin'}
    hashes = {key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in inputs.items()}
    for source in ['tone', 'silence']:
        cmd = [str(a.emulator), '--chip', chip, '--board', 'none', '--boot', 'rom',
               '--flash-mb', '4' if chip == 'c3' else '8', '--console', 'uart0',
               '--max-seconds', '1.2', '--no-dump']
        for flag, path in inputs.items():
            cmd += ['--' + flag, str(path)]
        if source == 'tone':
            cmd += ['--i2s-tone', '0:1000:0.5']
        run = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
        output = run.stdout.decode(errors='replace')
        samples = [(int(n), float(rms), float(hz)) for n, rms, hz in
                   re.findall(r'RX_BYTES=(\d+) RMS=([\d.]+) FREQ=([\d.]+)', output)]
        expected = 32767 * 0.5 / (2 ** 0.5)
        good = lambda x: x[0] == 512 and (abs(x[1] - expected) / expected <= .01 and
                                        abs(x[2] - 1000) <= 62.5 if source == 'tone'
                                        else x[1] == 0 and x[2] == 0)
        reset = output.find('[emu] chip reset')
        passed = (run.returncode == 0 and len(samples) >= 2 and all(map(good, samples))
                  and reset >= 0 and 'RX_BYTES=512' in output[reset:]
                  and 'RX_BEGIN=0' not in output and 'ARDUINO=3.3.8' in output)
        # Keep relevant serial and counters; omit ROM paths and unrelated boot output.
        serial = [line for line in output.splitlines()
                  if line.startswith(('ARDUINO=', 'RX_', '[emu] chip reset', '[emu] stop:'))]
        results.append(dict(environment=env, source=source, passed=passed, exit=run.returncode,
                            input_sha256=hashes, samples=samples, serial=serial,
                            output_sha256=hashlib.sha256(run.stdout).hexdigest()))
        print(env, source, 'PASS' if passed else 'FAIL', samples)
a.output.write_text(json.dumps(dict(emulator_sha256=hashlib.sha256(a.emulator.read_bytes()).hexdigest(),
                                   runs=results), indent=2) + '\n')
assert all(r['passed'] for r in results), 'Arduino RX check failed; see output receipt'

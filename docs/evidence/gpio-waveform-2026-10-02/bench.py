#!/usr/bin/env python3
"""Balanced native A/B runs. Arguments: baseline-binary candidate-binary output.json."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[3]
fw = root / 'web/wasm/fw'
binaries = dict(zip(('base', 'candidate'), sys.argv[1:3]))
results = {'binaries': {k: hashlib.sha256(Path(v).read_bytes()).hexdigest() for k, v in binaries.items()}, 'runs': []}
for workload, seconds, chip in [('panel', 5, 's3'), ('c3-hello', 30, 'c3'), ('c6-hello', 30, 'c6')]:
    config = json.loads((fw / (workload + '.json')).read_text())
    args = ['--chip', chip, '--boot', 'rom', '--no-dump', '--max-seconds', str(seconds), '--board', config['board'] if chip == 's3' else 'none', '--flash-mb', str(config['flash_mb'])]
    if chip == 's3':
        args += ['--psram-mb', str(config['psram_mb'])]
    for kind, name in config['files'].items():
        args += ['--' + kind, str(fw / name)]
    for address, name in config.get('flash_at', {}).items():
        args += ['--flash-at', address + '=' + str(fw / name)]
    reference = None
    for label in ['base', 'candidate', 'candidate', 'base'] * 2:
        start = time.perf_counter()
        run = subprocess.run([binaries[label], *args], capture_output=True, check=True)
        wall = time.perf_counter() - start
        # Only the guest-work clause, excluding host wall time and rate.
        work = re.search(rb'core0 (\d+) \+ core1 (\d+) insns', run.stderr)
        if work:
            counts = [int(x) for x in work.groups()]
        else:
            work = re.search(rb'\[emu\] stop:.*? (\d+) insns in', run.stderr)
            if not work:
                raise RuntimeError(run.stderr.decode()[-1000:])
            counts = [int(work.group(1))]
        output = hashlib.sha256(run.stdout).hexdigest()
        contract = (counts, output)
        if reference is None:
            reference = contract
        assert contract == reference, (workload, label, contract, reference)
        sample = dict(workload=workload, label=label, seconds=wall, instructions=counts, stdout_sha256=output)
        results['runs'].append(sample)
        print(json.dumps(sample), flush=True)
        Path(sys.argv[3]).write_text(json.dumps(results, indent=2) + '\n')

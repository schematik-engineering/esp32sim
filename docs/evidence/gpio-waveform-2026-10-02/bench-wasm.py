#!/usr/bin/env python3
"""Use the existing WASM smoke driver for balanced Node A/B samples.
Arguments: baseline.wasm candidate.wasm output.json. Run after other checks finish.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[3]
binaries = dict(zip(('base', 'candidate'), sys.argv[1:3]))
results = {'binaries': {k: hashlib.sha256(Path(v).read_bytes()).hexdigest() for k, v in binaries.items()}, 'runs': []}
for label in ['base', 'candidate', 'candidate', 'base'] * 2:
    start = time.perf_counter()
    run = subprocess.run(['node', 'tools/wasm-test.mjs', 'hello', 'c3-hello', 'c6-hello', 'panel'], cwd=root, env={**os.environ, 'ESP32SIM_WASM': str(Path(binaries[label]).resolve())}, capture_output=True, text=True, check=True)
    wall = time.perf_counter() - start
    lines = [x for x in run.stdout.splitlines() if x.startswith('ok   ')]
    assert len(lines) == 5, run.stdout
    contract = [re.sub(r' in [0-9.]+ s wall \([0-9.]+ Minsn/s\)', '', x) for x in lines]
    if results['runs']:
        assert contract == results['runs'][0]['checks'], (contract, results['runs'][0]['checks'])
    sample = dict(label=label, seconds=wall, checks=contract)
    results['runs'].append(sample)
    Path(sys.argv[3]).write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(sample), flush=True)

#!/usr/bin/env python3
"""Alternating user-CPU pairs for two esp32sim builds (CI port of retire/bench/run.py).

    bench.py --config bench.json --base BINDIR --head BINDIR --fw TREE_ROOT --out DIR --label LABEL
    bench.py --self-test

Method (same as run.py / the maintainer's cpu_pairs.py): per workload one warmup pair
(head, base), then PAIRS measured pairs alternating base->head and head->base. User CPU of each
run is the child's ru_utime from getrusage(RUSAGE_CHILDREN) around one sequential child (the
same counter /usr/bin/time reports; no wrapper process in the timed child). stdout goes to a
file; its SHA-256 is the console hash. Every run, warmup included, must agree on exit code 0,
the emulated duration, per-core/total instructions and the console hash, otherwise the workload
is a MISMATCH, has no performance numbers, and the script exits non-zero.
The change is 100 * (head median / base median - 1); flag = head slower in >= 6 of 7 pairs.
"""
import argparse, hashlib, json, os, platform, re, resource, statistics, subprocess, sys, time
from pathlib import Path

FW, LOCAL = 'web/wasm/fw/public', 'web/wasm/fw/local'
ROM = 'web/wasm/fw'
NAMES = {'s3-hello': 'S3 hello', 'c3-hello': 'C3 hello', 'c6-hello': 'C6 hello', 'pocket-tank': 'Pocket Tank',
         'classic-hello': 'Classic hello', 'tdisplay-prices': 'T-Display MicroPython', 'c6-wifi-station': 'C6 Wi-Fi station'}
DEFAULT_SECONDS = {'s3-hello': 30, 'c3-hello': 30, 'c6-hello': 30, 'pocket-tank': 8, 'classic-hello': 30,
                   'tdisplay-prices': 15, 'c6-wifi-station': 14}


def workload(name, seconds):
    """(binary, args) exactly as cpu_pairs.py / run.py spell them; paths relative to the base tree root."""
    s = str(seconds)
    r = lambda n: f'{ROM}/{n}'
    if name == 's3-hello':
        return 'esp32sim', ['--boot', 'rom', '--rom', r('esp32s3_rev0_rom.elf'), '--board', 'none', '--bootloader', f'{FW}/hello-bootloader.bin',
                            '--ptable', f'{FW}/hello-ptable.bin', '--app', f'{FW}/hello_world.bin', '--max-seconds', s, '--no-dump']
    if name == 'c3-hello':
        return 'esp32sim-c3', ['--boot', 'rom', '--rom', r('esp32c3_rev3_rom.elf'), '--flash-mb', '4', '--bootloader', f'{FW}/c3-hello-bootloader.bin',
                               '--ptable', f'{FW}/c3-hello-ptable.bin', '--app', f'{FW}/c3-hello_world.bin', '--max-seconds', s, '--no-dump']
    if name == 'c6-hello':
        return 'esp32sim-c6', ['--boot', 'rom', '--rom', r('esp32c6_rev0_rom.elf'), '--flash-mb', '4', '--bootloader', f'{FW}/c6-hello-bootloader.bin',
                               '--ptable', f'{FW}/c6-hello-ptable.bin', '--app', f'{FW}/c6-hello_world.bin', '--max-seconds', s, '--no-dump']
    if name == 'pocket-tank':
        return 'esp32sim', ['--boot', 'rom', '--rom', r('esp32s3_rev0_rom.elf'), '--board', 'waveshare-amoled18-v2', '--flash-mb', '16', '--psram-mb', '8',
                            '--bootloader', f'{FW}/pocket-tank-bootloader.bin', '--ptable', f'{FW}/pocket-tank-ptable.bin', '--app', f'{FW}/pocket-tank.bin',
                            '--flash-at', f'0x290000={LOCAL}/model_q4.bin', '--max-seconds', s, '--no-dump']
    if name == 'classic-hello':
        return 'esp32sim', ['--chip', 'esp32', '--rom', r('esp32_rev300_rom.elf'), '--board', 'esp32dev', '--boot', 'rom', '--no-dump', '--console', 'uart0',
                            '--bootloader', f'{FW}/classic-hello-bootloader.bin', '--ptable', f'{FW}/classic-hello-ptable.bin',
                            '--app', f'{FW}/classic-hello_world.bin', '--max-seconds', s]
    if name == 'tdisplay-prices':
        return 'esp32sim', ['--chip', 'esp32', '--board', 'ttgo-tdisplay', '--rom', r('esp32_rev300_rom.elf'), '--boot', 'rom', '--console', 'uart0',
                            '--flash-mb', '4', '--flash-at', f'0x1000={ROM}/ESP32_GENERIC-20260824-v1.29.0.bin',
                            '--flash-at', f'0x200000={FW}/tdisplay-vfs.bin', '--max-seconds', s, '--no-dump']
    if name == 'c6-wifi-station':
        return 'esp32sim-c6', ['--rom', r('esp32c6_rev0_rom.elf'), '--boot', 'rom', '--flash-mb', '4', '--console', 'usb', '--no-dump',
                               '--bootloader', f'{FW}/c6-wifi-bootloader.bin', '--ptable', f'{FW}/c6-wifi-ptable.bin', '--app', f'{FW}/c6-wifi_station.bin',
                               '--wifi', 'ssid=esp32sim,psk=esp32sim-pass', '--net', 'none', '--max-seconds', s]
    raise SystemExit(f'unknown workload {name}')


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def parse(stderr):
    line = next(l for l in stderr.splitlines() if '[emu] stop:' in l)
    cores = [int(n) for n in re.findall(r'core\d+ (\d+)', line)]
    total = sum(cores) if cores else int(re.search(r'(\d+) insns', line)[1])
    seconds = float(re.search(r'emulated ([\d.]+)s', line)[1])
    return total, cores, seconds, line


def run(binary, args, cwd, raw, prefix, seconds):
    out, err = raw / f'{prefix}.stdout', raw / f'{prefix}.stderr'
    load = list(os.getloadavg()); t0 = time.monotonic(); before = resource.getrusage(resource.RUSAGE_CHILDREN)
    with out.open('wb') as o, err.open('wb') as e:
        proc = subprocess.run([str(binary), *args], cwd=cwd, stdin=subprocess.DEVNULL, stdout=o, stderr=e)
    after = resource.getrusage(resource.RUSAGE_CHILDREN); wall = time.monotonic() - t0
    s = {'user_seconds': after.ru_utime - before.ru_utime, 'system_seconds': after.ru_stime - before.ru_stime, 'wall_seconds': wall,
         'load_before': load, 'load_after': list(os.getloadavg()), 'exit_code': proc.returncode, 'console_sha256': sha(out)}
    text = err.read_text(errors='replace')
    try:
        total, cores, emulated, line = parse(text)
        s.update(instructions=total, core_instructions=cores, emulated_seconds=emulated, stop=line)
        if proc.returncode or abs(emulated - seconds) > 0.001:
            s['error'] = 'nonzero exit or incomplete emulated duration'
    except (StopIteration, TypeError, ValueError):
        s['error'] = 'missing or invalid stop line: ' + text[-400:]
    return s


def quartiles(v):
    q = statistics.quantiles(v, n=4, method='inclusive')
    return q[0], q[2]


def stats(pairs, npairs):
    meas = [p for p in pairs if not p['warmup']]
    res = {}
    for a in ('base', 'head'):
        v = [p[a]['user_seconds'] for p in meas]
        q1, q3 = quartiles(v); med = statistics.median(v)
        res[a] = {'median': med, 'min': min(v), 'max': max(v), 'q1': q1, 'q3': q3, 'iqr_over_median_pct': 100 * (q3 - q1) / med if med else None}
    res['change_percent'] = 100 * (res['head']['median'] / res['base']['median'] - 1)
    res['head_slower_pairs'] = sum(p['head']['user_seconds'] > p['base']['user_seconds'] for p in meas)
    res['pairs'] = npairs
    # per-pair ratio spread: how noisy a single paired comparison is on this machine
    ratios = [100 * (p['head']['user_seconds'] / p['base']['user_seconds'] - 1) for p in meas]
    q1, q3 = quartiles(ratios)
    res['pair_delta_pct'] = {'median': statistics.median(ratios), 'q1': q1, 'q3': q3, 'min': min(ratios), 'max': max(ratios)}
    res['nonoverlap'] = res['base']['max'] < res['head']['min'] or res['head']['max'] < res['base']['min']
    # charter flag: head slower in >= 6 of 7 pairs; also flag when every head run is slower than every base run
    res['flag'] = res['head_slower_pairs'] >= 6 or res['head']['min'] > res['base']['max']
    return res


def environment():
    def cmd(c):
        try: return subprocess.check_output(c, text=True, stderr=subprocess.DEVNULL).strip()
        except Exception: return None
    if sys.platform == 'darwin':
        cpu = cmd(['sysctl', '-n', 'machdep.cpu.brand_string'])
    else:
        cpu = cmd(['sh', '-c', "lscpu | sed -n 's/^Model name:[ ]*//p' | head -1"]) or cmd(['sh', '-c', "grep -m1 -i 'model name' /proc/cpuinfo"])
    return {'runner_os': os.environ.get('RUNNER_OS'), 'runner_arch': os.environ.get('RUNNER_ARCH'), 'runner_label': os.environ.get('BENCH_RUNNER'),
            'image_os': os.environ.get('ImageOS'), 'image_version': os.environ.get('ImageVersion'), 'cpu_model': cpu, 'cpu_count': os.cpu_count(),
            'system': platform.system(), 'release': platform.release(), 'machine': platform.machine(),
            'rustc': os.environ.get('BENCH_RUSTC_VV') or cmd(['rustc', '-Vv'])}


def fmt(v):
    return f"{v['median']:.3f} ({v['min']:.3f}–{v['max']:.3f})"


def report(r):
    e = r['environment']
    lines = [f"### CPU bench `{r['label']}`", '',
             f"Base `{r['base_sha'][:8]}` vs head `{r['head_sha'][:8]}`. Runner `{e['runner_label']}` (image {e['image_os']} {e['image_version']}), "
             f"CPU {e['cpu_model']} ×{e['cpu_count']}, rustc `{(e['rustc'] or '').splitlines()[0] if e['rustc'] else '?'}`.",
             f"User CPU s, 1 warmup + {r['pairs']} alternating pairs, identical instructions and console hashes required.", '',
             '| Workload | Base median (range) | Head median (range) | Change | Head slower | IQR/median base / head | Instructions | Flag |',
             '| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |']
    for name, w in r['workloads'].items():
        label = f"{NAMES.get(name, name)}, {w['seconds']} s"
        if 'stats' not in w:
            lines.append(f"| {label} | — | — | **{w['status']}** | — | — | — | — |"); continue
        s = w['stats']; ins = w['pairs'][0]['base']['instructions']
        flag = 'slower ≥6/7' if s['head_slower_pairs'] >= 6 else ''
        if s['nonoverlap']: flag = (flag + '; ' if flag else '') + 'ranges do not overlap'
        lines.append(f"| {label} | {fmt(s['base'])} | {fmt(s['head'])} | {s['change_percent']:+.2f}% | {s['head_slower_pairs']}/{r['pairs']} | "
                     f"{s['base']['iqr_over_median_pct']:.2f}% / {s['head']['iqr_over_median_pct']:.2f}% | {ins:,} | {flag or '—'} |")
    return '\n'.join(lines) + '\n'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--config', required=True); ap.add_argument('--base', required=True); ap.add_argument('--head', required=True)
    ap.add_argument('--fw', required=True); ap.add_argument('--out', required=True); ap.add_argument('--label', required=True)
    a = ap.parse_args()
    cfg = json.loads(Path(a.config).read_text())
    npairs, nwarm = int(cfg.get('pairs', 7)), int(cfg.get('warmup', 1))
    out = Path(a.out); raw = out / 'raw'; raw.mkdir(parents=True, exist_ok=True)
    cwd = Path(a.fw).resolve(); base, head = Path(a.base).resolve(), Path(a.head).resolve()
    r = {'label': a.label, 'base_sha': cfg['base'], 'head_sha': cfg['head'], 'toolchain': cfg.get('toolchain'), 'pairs': npairs, 'warmup': nwarm,
         'environment': environment(), 'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'workloads': {}}
    bad = False
    for spec in cfg['workloads']:
        name = spec if isinstance(spec, str) else spec['name']
        seconds = DEFAULT_SECONDS[name] if isinstance(spec, str) else spec.get('seconds', DEFAULT_SECONDS[name])
        exe, args = workload(name, seconds)
        bins = {'base': base / exe, 'head': head / exe}
        w = {'seconds': seconds, 'binary': exe, 'args': args, 'binary_sha256': {k: sha(v) for k, v in bins.items()}, 'pairs': [], 'status': 'RUNNING'}
        r['workloads'][name] = w; expected = None
        order_list = [('warmup', ['head', 'base'])] * nwarm + [(i, ['base', 'head'] if i % 2 else ['head', 'base']) for i in range(1, npairs + 1)]
        for idx, (pair, order) in enumerate(order_list):
            p = {'pair': pair, 'warmup': pair == 'warmup', 'order': order}; w['pairs'].append(p)
            for side in order:
                s = run(bins[side], args, cwd, raw, f'{name}-{idx}-{side}', seconds); p[side] = s
                print(f"  {name} {str(pair):>6} {side:>4}: {s['user_seconds']:8.3f} s  {s.get('core_instructions') or s.get('instructions')}", file=sys.stderr, flush=True)
                key = (s.get('instructions'), tuple(s.get('core_instructions') or ()), s['console_sha256'])
                if expected is None: expected = key
                if s.get('error') or key != expected:
                    w['status'] = 'MISMATCH'; w['error'] = f"pair {pair} {side}: " + s.get('error', f'expected {expected}, got {key}'); break
            if w['status'] == 'MISMATCH': break
        if w['status'] == 'MISMATCH':
            bad = True
        else:
            w['status'] = 'PASS'; w['stats'] = stats(w['pairs'], npairs)
            w['console_sha256'] = expected[2]
        print(f"{name}: {w['status']} {json.dumps(w.get('stats', {}).get('change_percent'))}", file=sys.stderr, flush=True)
    r['finished_utc'] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    (out / 'result.json').write_text(json.dumps(r, indent=1) + '\n')
    md = report(r); (out / 'result.md').write_text(md)
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as f: f.write(md + '\n')
    print(md)
    sys.exit(1 if bad else 0)


def self_test():
    assert parse('[emu] stop: Limit — core0 12 + core1 3 insns in 0.1s wall = 0 Minsn/s; emulated 30.000s (15 cycles); 0 exceptions')[:3] == (15, [12, 3], 30.0)
    assert parse('[emu] stop: Limit — 42 insns in 0.1s wall = 0 Minsn/s; emulated 3.000s (42 cycles); 0 exceptions')[:3] == (42, [], 3.0)
    ps = [{'warmup': False, 'base': {'user_seconds': x}, 'head': {'user_seconds': 2 * x}} for x in range(1, 8)]
    s = stats(ps, 7); assert s['change_percent'] == 100 and s['head_slower_pairs'] == 7 and not s['nonoverlap'] and s['flag']
    ps = [{'warmup': False, 'base': {'user_seconds': 1.0}, 'head': {'user_seconds': 0.5}} for _ in range(7)]
    s = stats(ps, 7); assert s['change_percent'] == -50 and s['head_slower_pairs'] == 0 and s['nonoverlap'] and s['base']['iqr_over_median_pct'] == 0 and not s['flag']
    print('self-test passed')


if __name__ == '__main__':
    self_test() if '--self-test' in sys.argv else main()

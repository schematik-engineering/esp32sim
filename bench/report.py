#!/usr/bin/env python3
"""Aggregate every result.json of one bench run (all runners x repetitions) into one table.

    report.py RESULTS_DIR CONFIG [OUT_PREFIX]

Adds, per runner and workload, the spread across repetitions and, when bench.json has a
`reference` block (the maintainer's table), whether direction and flag status match it.
"""
import json, statistics, sys
from pathlib import Path

NAMES = {'s3-hello': 'S3 hello', 'c3-hello': 'C3 hello', 'c6-hello': 'C6 hello', 'pocket-tank': 'Pocket Tank',
         'classic-hello': 'Classic hello', 'tdisplay-prices': 'T-Display MicroPython', 'c6-wifi-station': 'C6 Wi-Fi station'}
NEUTRAL = 1.0  # |change| below this in the reference counts as "no change"


def verdict(ref, s):
    """Same direction and same flag status as the reference table."""
    rflag = ref['slower'] >= 6
    oflag = s['flag']
    if abs(ref['change']) < NEUTRAL:
        direction = abs(s['change_percent']) < 3.0  # neutral reference: ours must stay within the ±3% we claim to resolve
    else:
        direction = (ref['change'] > 0) == (s['change_percent'] > 0)
    return direction and rflag == oflag, direction, rflag, oflag


def main():
    root = Path(sys.argv[1]); cfg = json.loads(Path(sys.argv[2]).read_text())
    prefix = Path(sys.argv[3]) if len(sys.argv) > 3 else root / 'summary'
    results = sorted((json.loads(p.read_text()) for p in root.rglob('result.json')), key=lambda r: r['label'])
    refs = cfg.get('reference', {})
    lines = [f"## CPU bench {cfg.get('name', '')}: base `{cfg['base'][:8]}` vs head `{cfg['head'][:8]}`", '',
             f"Toolchain {cfg.get('toolchain')}; {cfg.get('warmup', 1)} warmup + {cfg.get('pairs', 7)} alternating pairs per run; user CPU seconds.", '',
             '| Runner / rep | CPU | Workload | Base median | Head median | Change | Head slower | Pair Δ IQR (q1..q3) | IQR/median base/head | Flag | Reference | Match |',
             '| --- | --- | --- | ---: | ---: | ---: | ---: | --- | ---: | --- | --- | --- |']
    agg = {}
    for r in results:
        e = r['environment']
        for name, w in r['workloads'].items():
            ref = refs.get(name)
            reftxt = f"{ref['change']:+.1f}%, {ref['slower']}/7" if ref else '—'
            if 'stats' not in w:
                lines.append(f"| {r['label']} | {e['cpu_model']} | {NAMES.get(name, name)} {w['seconds']} s | — | — | **{w['status']}** | — | — | — | — | {reftxt} | no |"); continue
            s = w['stats']; pd = s['pair_delta_pct']
            m = verdict(ref, s)[0] if ref else None
            agg.setdefault((e['runner_label'], name), []).append(s)
            lines.append(f"| {r['label']} | {e['cpu_model']} | {NAMES.get(name, name)} {w['seconds']} s | {s['base']['median']:.3f} | {s['head']['median']:.3f} | "
                         f"{s['change_percent']:+.2f}% | {s['head_slower_pairs']}/{s['pairs']} | {pd['q1']:+.2f}..{pd['q3']:+.2f}% | "
                         f"{s['base']['iqr_over_median_pct']:.2f}/{s['head']['iqr_over_median_pct']:.2f}% | {'**FLAG**' if s['flag'] else '—'} | {reftxt} | "
                         f"{'—' if m is None else ('yes' if m else '**no**')} |")
    lines += ['', '| Runner | Workload | Reps | Change per rep | Rep spread (max−min) | Max pair-Δ IQR | Usable for ±3% |', '| --- | --- | ---: | --- | ---: | ---: | --- |']
    usable = {}
    for (runner, name), ss in sorted(agg.items()):
        ch = [s['change_percent'] for s in ss]; spread = max(ch) - min(ch)
        iqr = max(s['pair_delta_pct']['q3'] - s['pair_delta_pct']['q1'] for s in ss)
        ok = iqr <= 2.0 and spread <= 1.5
        usable[f'{runner}/{name}'] = {'changes': ch, 'rep_spread_pct': spread, 'max_pair_delta_iqr_pct': iqr, 'usable_3pct': ok}
        lines.append(f"| {runner} | {NAMES.get(name, name)} | {len(ss)} | {', '.join(f'{c:+.2f}%' for c in ch)} | {spread:.2f} pp | {iqr:.2f} pp | {'yes' if ok else '**no**'} |")
    lines += ['', 'Usable for ±3%: pair-Δ IQR ≤ 2.0 pp in every repetition and repetitions agree within 1.5 pp. Flag = head slower in ≥ 6/7 pairs or every head run slower than every base run.']
    hc = [(r['label'], name, w['stats']) for r in results for name, w in r['workloads'].items() if 'host_instructions' in w.get('stats', {})]
    if hc:
        lines += ['', 'Host counters from `/usr/bin/time -l` (screening only; the table metric stays user CPU):', '',
                  '| Runner / rep | Workload | Host instructions Δ | IQR/median base/head | Host cycles Δ | IQR/median base/head |', '| --- | --- | ---: | ---: | ---: | ---: |']
        for label, name, s in hc:
            i, c = s['host_instructions'], s.get('host_cycles', {})
            lines.append(f"| {label} | {NAMES.get(name, name)} | {i['change_percent']:+.3f}% | {i['base_iqr_over_median_pct']:.3f}/{i['head_iqr_over_median_pct']:.3f}% | "
                         + (f"{c['change_percent']:+.2f}% | {c['base_iqr_over_median_pct']:.2f}/{c['head_iqr_over_median_pct']:.2f}% |" if c else '— | — |'))
    md = '\n'.join(lines) + '\n'
    prefix.with_suffix('.md').write_text(md)
    prefix.with_suffix('.json').write_text(json.dumps({'config': cfg, 'usable': usable, 'runs': results}, indent=1) + '\n')
    print(md)


if __name__ == '__main__':
    main()

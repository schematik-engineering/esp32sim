#!/usr/bin/env python3
"""Extract pinned linear Zb sites; no reachability or execution claim."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import audit_isa


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    for name in ('app', 'bootloader', 'objdump', 'objcopy', 'work', 'out'):
        ap.add_argument('--'+name, required=True, type=Path)
    a = ap.parse_args()
    for name in ('app', 'bootloader'):
        assert hashlib.sha256(getattr(a, name).read_bytes()).hexdigest() == audit_isa.PINS[name]
    a.work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    boot, _ = audit_isa.boot_elf(a.bootloader, a.app, a.objcopy, a.work)
    inventory = json.loads((Path(__file__).parent/'m1a-isa.json').read_text())
    for name, path in [('bootloader', boot), ('app', a.app)]:
        rows, _ = audit_isa.disassemble(a.objdump, path)
        rows = [r for r in rows.values() if r['isa'] in ('Zba', 'Zbb', 'Zbs')]
        expected = next(s for s in inventory['stages'] if s['name'] == name)
        for cls in ('Zba', 'Zbb', 'Zbs'):
            assert dict(Counter(r['mnemonic'] for r in rows if r['isa'] == cls)) == expected['mnemonics'][cls]
        (a.out/(name+'.tsv')).write_text(''.join(f"{r['pc']:08x}\t{r['raw']:08x}\t{r['mnemonic']}\n" for r in rows))
        print(name, len(rows), dict(sorted(Counter(r['mnemonic'] for r in rows).items())))


if __name__ == '__main__':
    main()

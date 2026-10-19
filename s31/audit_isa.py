#!/usr/bin/env python3
"""Offline, pinned static ISA inventory. Never confuses reachability with execution."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import re
import struct
import subprocess

PINS = {
    'rom': 'a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87',
    'app': 'c16d55efdd65676282af02b39f879a0b7f3ed62ec56ce919c606895a3f82ae12',
    'bootloader': 'd38e48ad52800c79ef97613f9ab452296fbcbd731ef51a0f46919978adc9b664',
}
CLASSES = ['I', 'M', 'A', 'F', 'C/Zca', 'Zcf', 'Zcb', 'Zcmp', 'Zcmt', 'Zba', 'Zbb', 'Zbc', 'Zbs', 'Zicsr', 'Zifencei', 'XespV', 'XespLoop CSR', 'privileged', 'unknown/data']
ZCB = set('c.lbu c.lhu c.lh c.sb c.sh c.zext.b c.sext.b c.zext.h c.sext.h c.zext.w c.not c.mul'.split())
ZBB = set('andn orn xnor clz ctz cpop max maxu min minu sext.b sext.h zext.h rol ror rori orc.b rev8'.split())
ZBS = set('bclr bclri bext bexti binv binvi bset bseti'.split())

def isa(m, raw):
    if m.startswith('csrr') and 0x7c6 <= raw >> 20 <= 0x7cb: return 'XespLoop CSR'
    if m.startswith('csrr'): return 'Zicsr'
    if m == 'fence.i': return 'Zifencei'
    if m in ('mret', 'sret', 'wfi', 'sfence.vma'): return 'privileged'
    if m.startswith('esp.'): return 'XespV'
    if m.startswith(('lp.', 'esp.lp')): return 'XespLoop CSR'
    if m.startswith('cm.j'): return 'Zcmt'
    if m.startswith('cm.'): return 'Zcmp'
    if m in ZCB: return 'Zcb'
    if m.startswith('c.f'): return 'Zcf'
    if m == 'c.unimp' or m.startswith('.'): return 'unknown/data'
    if m.startswith('c.'): return 'C/Zca'
    if m in ('sh1add', 'sh2add', 'sh3add'): return 'Zba'
    if m in ZBB: return 'Zbb'
    if m in ZBS: return 'Zbs'
    if m.startswith('clmul'): return 'Zbc'
    if m.startswith(('amo', 'lr.w', 'sc.w')): return 'A'
    if m.startswith(('mul', 'div', 'rem')): return 'M'
    if m.startswith('f') and m != 'fence': return 'F'
    return 'I'

def disassemble(objdump, path):
    text = subprocess.check_output([str(objdump), '-d', '-M', 'no-aliases', str(path)], text=True)
    rows, symbols = {}, {}
    for line in text.splitlines():
        s = re.match(r'^([0-9a-f]+) <(.*)>:', line)
        if s: symbols[s[2]] = int(s[1], 16)
        m = re.match(r'^\s*([0-9a-f]+):\s+([0-9a-f]+)\s+(\S+)(?:\s+(.*))?$', line)
        if m:
            pc, raw, mnemonic, operands = int(m[1], 16), int(m[2], 16), m[3], m[4] or ''
            rows[pc] = dict(pc=pc, raw=raw, length=len(m[2])//2, mnemonic=mnemonic, operands=operands, isa=isa(mnemonic, raw))
    return rows, symbols

def boot_elf(boot, app, objcopy, work):
    # Reuse the exact hello .riscv.attributes. Raw binary disassembly defaults
    # to D/RV64 and misreads Zcmp as c.fsdsp/c.addiw, so it is NOT usable here.
    b = boot.read_bytes()
    if b[0] != 0xe9 or struct.unpack_from('<H', b, 12)[0] != 32: raise ValueError('not S31 ESP image')
    args = [str(objcopy), '--only-section=.riscv.attributes']
    off = 24
    for i in range(b[1]):
        addr, size = struct.unpack_from('<II', b, off); off += 8
        payload = b[off:off+size]; off += size
        if len(payload) != size: raise ValueError('truncated boot image')
        # M0 first segment is rodata; code is in the other two load segments.
        if i == 0: continue
        p = work / f'boot-{i}.bin'; p.write_bytes(payload)
        name = f'.boot{i}'
        args += [f'--add-section={name}={p}', f'--set-section-flags={name}=alloc,load,readonly,code', f'--change-section-vma={name}=0x{addr:x}']
    out = work / 'boot-audit.elf'
    subprocess.run(args + [str(app), str(out)], check=True, capture_output=True)
    return out, struct.unpack_from('<I', b, 4)[0]

def reachable(rows, roots):
    seen, unresolved, todo = set(), set(), list(roots)
    while todo:
        pc = todo.pop()
        if pc in seen: continue
        if pc not in rows: unresolved.add((pc, 'target outside disassembly')); continue
        seen.add(pc)
        r = rows[pc]; m, ops = r['mnemonic'], r['operands']; nxt = pc + r['length']
        targets = re.findall(r'(?:^|,)(?:0x)?([0-9a-f]{6,8})(?:\s|$)', ops)
        target = int(targets[0], 16) if targets else None
        if m in ('jal', 'c.j', 'c.jal'):
            if target is not None: todo.append(target)
            else: unresolved.add((pc, 'direct target missing'))
            if m == 'c.jal' or (m == 'jal' and not ops.startswith('zero,')): todo.append(nxt)
        elif m in ('beq', 'bne', 'blt', 'bltu', 'bge', 'bgeu', 'c.beqz', 'c.bnez'):
            if target is not None: todo.append(target)
            todo.append(nxt)
        elif m == 'jalr':
            # GNU prints an absolute annotation for an adjacent AUIPC/JALR pair.
            prev = rows.get(pc-4, {})
            comment = re.search(r'#\s*(?:0x)?([0-9a-f]{6,8})\b', ops)
            if prev.get('mnemonic') == 'auipc' and comment: todo.append(int(comment[1], 16))
            else: unresolved.add((pc, 'indirect jalr'))
            if not ops.startswith('zero,'): todo.append(nxt)
        elif m in ('c.jr', 'mret', 'sret', 'cm.popret', 'cm.popretz'):
            pass
        elif m in ('c.jalr', 'cm.jt', 'cm.jalt'):
            unresolved.add((pc, 'indirect control transfer'))
            if m != 'cm.jt': todo.append(nxt)
        elif r['isa'] == 'unknown/data':
            unresolved.add((pc, 'unknown/data boundary'))
        else: todo.append(nxt)
    return seen, unresolved

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    for name in ('rom', 'app', 'bootloader', 'objdump', 'objcopy', 'probe', 'work', 'out'):
        ap.add_argument('--'+name, type=Path, required=True)
    a = ap.parse_args(); a.work.mkdir(parents=True, exist_ok=True); a.out.mkdir(parents=True, exist_ok=True)
    for name, pin in PINS.items():
        if hashlib.sha256(getattr(a, name).read_bytes()).hexdigest() != pin: raise ValueError(f'{name} hash differs from M0 pin')
    boot, boot_entry = boot_elf(a.bootloader, a.app, a.objcopy, a.work)
    stages = []
    for name, path, root_names in [
        ('rom', a.rom, ['_start', 'ets_printf', 'ets_run', 'ets_run_flash_bootloader']),
        ('bootloader', boot, []), ('app', a.app, ['call_start_cpu0', 'app_main', 'main_task']),
    ]:
        rows, symbols = disassemble(a.objdump, path)
        if name == 'bootloader': roots = {'image_entry': boot_entry}
        else:
            roots = {n: symbols[n] for n in root_names if n in symbols}
            if len(roots) != len(root_names): raise ValueError(f'missing roots {set(root_names)-roots.keys()}')
        seen, unresolved = reachable(rows, roots.values())
        ordered = sorted(rows.values(), key=lambda r: r['pc'])
        feed = ''.join(f"{r['pc']:x} {r['raw']:x}\n" for r in ordered)
        result = subprocess.run([str(a.probe), '--decode-stdin'], input=feed, text=True, capture_output=True, check=True)
        decoded = [line.split('\t') for line in result.stdout.splitlines()]
        if len(decoded) != len(ordered): raise ValueError('decoder record count differs')
        aggregate = defaultdict(Counter); mnemonics = defaultdict(Counter); examples = {}
        for r, d in zip(ordered, decoded):
            assert int(d[0], 16) == r['pc'] and int(d[1], 16) == r['raw']
            cls = r['isa']; status = 'illegal' if d[3] == 'Illegal' else 'accepted'
            aggregate[cls]['all'] += 1; aggregate[cls]['all_'+status] += 1
            mnemonics[cls][r['mnemonic']] += 1
            if r['pc'] in seen:
                aggregate[cls]['candidate'] += 1; aggregate[cls]['candidate_'+status] += 1
                if status == 'illegal': examples.setdefault(cls, f"0x{r['pc']:08x}: {r['raw']:0{r['length']*2}x} {r['mnemonic']} {r['operands'].split('#')[0].strip()}")
        root_instructions = {n: f"0x{pc:08x}: {rows[pc]['raw']:0{rows[pc]['length']*2}x} {rows[pc]['mnemonic']} {rows[pc]['operands'].split('#')[0].strip()}" for n,pc in roots.items()}
        stages.append(dict(name=name, roots={n: f'0x{p:08x}' for n,p in roots.items()}, root_instructions=root_instructions, total=len(rows), candidate=len(seen),
                           unresolved_count=len(unresolved), unresolved_examples=[f'0x{pc:08x}: {why}' for pc,why in sorted(unresolved)[:12]],
                           counts={k: dict(v) for k,v in sorted(aggregate.items())}, mnemonics={k: dict(sorted(v.items())) for k,v in sorted(mnemonics.items())}, unsupported_examples=examples))
    data = dict(input_sha256=PINS, objdump_version=subprocess.check_output([str(a.objdump), '--version'], text=True).splitlines()[0], stages=stages)
    (a.out/'m1a-isa.json').write_text(json.dumps(data, indent=2, sort_keys=True)+'\n')
    lines = ['# S31 M1a static ISA inventory', '', 'Generated by `s31/audit_isa.py`; one count per unique instruction address, not execution frequency.',
             'Static candidates are NOT instructions proven executed. Branches include both arms, calls include return continuations, and selected stage roots are explicit seeds.',
             'Indirect control flow, CLIC vectors, scheduler targets, conditional callbacks and data-in-code prevent a complete minimum-executed-ISA claim. Zero means absent in this inventory, not impossible at runtime.',
             'The only dynamic result in this slice is six retired reset-prefix instructions followed by the first MMIO stop; no banner, second stage or application executes yet.', '',
             '## Class counts', '', 'Each cell: static candidate sites / rejected by the existing C3/C6 decoder. Full linear inventories and mnemonic counts are in m1a-isa.json.', '',
             '| Class | ROM | Second stage | Application | Decoder/semantics |', '|---|---:|---:|---:|---|']
    supported = {'I','M','A','C/Zca','Zicsr','Zifencei','privileged','XespLoop CSR'}
    for cls in CLASSES:
        cells = [f"{s['counts'].get(cls, {}).get('candidate', 0)} / {s['counts'].get(cls, {}).get('candidate_illegal', 0)}" for s in stages]
        status = 'decoded' if cls in supported else 'not implemented'
        if cls == 'Zicsr': status = 'encoding decoded; S31 CSR semantics not generally implemented'
        if cls == 'XespLoop CSR': status = 'CSR encoding decoded, loop semantics NOT implemented'
        if cls == 'privileged': status = 'C3/C6 semantics only; no S31 CLIC/Sv32'
        if cls == 'unknown/data': status = 'unknown/data/padding; not an extension count'
        lines.append('| '+ ' | '.join([cls]+cells+[status])+' |')
    lines += ['', '## Full linear inventory (not a boot-path execution claim)', '',
              'Each cell: all executable-section disassembly rows / decoder rejections. Embedded data and unused routines can appear here. This table keeps F, Zcf, vendor vector and loop-context code visible even when rooted static traversal does not reach it.', '',
              '| Class | ROM | Second stage | Application |', '|---|---:|---:|---:|']
    for cls in CLASSES:
        cells = [f"{s['counts'].get(cls, {}).get('all', 0)} / {s['counts'].get(cls, {}).get('all_illegal', 0)}" for s in stages]
        lines.append('| ' + ' | '.join([cls] + cells) + ' |')
    lines += ['', '## Dynamically exercised reset prefix', '',
              '| PC | Raw | Instruction | Class | Result |', '|---|---|---|---|---|',
              '| 0x2f800000 | 7100106f | jal zero,0x2f801710 | I | retired |',
              '| 0x2f801710 | 30001073 | csrrw zero,mstatus,zero | Zicsr | retired |',
              '| 0x2f801714 | f14022f3 | csrrs t0,mhartid,zero | Zicsr | retired, core 0 |',
              '| 0x2f801718 | 02029363 | bne t0,zero,0x2f80173e | I | retired, not taken |',
              '| 0x2f80171c | 20701337 | lui t1,0x20701 | I | retired |',
              '| 0x2f801720 | 03030313 | addi t1,t1,48 | I | retired |',
              '| 0x2f801724 | 00032303 | lw t1,0(t1) | I | STOP: unimplemented MMIO read 0x20701030, 4 bytes |',
              '', 'Executed counts: I=4, Zicsr=2; faulting attempt: I=1. All other classes=0 in this reset-prefix run. No extrapolation to the rest of boot.',
              '', '## Rooted static candidates and boundaries', '']
    for s in stages:
        lines += [f"### {s['name']}", '', ', '.join(n+'='+p for n,p in s['roots'].items()),
                  f"{s['candidate']} candidate sites from {s['total']} linear disassembly rows; {s['unresolved_count']} unresolved edges/boundaries.", '',
                  'Root entry instructions:']
        lines += ['- '+n+': `'+v+'`' for n,v in s['root_instructions'].items()]
        lines += ['', 'Unsupported examples:']
        lines += ['- '+k+': `'+v+'`' for k,v in sorted(s['unsupported_examples'].items())]
        lines += ['', 'First unresolved examples:'] + ['- '+v for v in s['unresolved_examples']] + ['']
    lines += ['## Pinned input SHA256', ''] + ['- '+n+': `'+h+'`' for n,h in PINS.items()]
    lines += ['', 'Bootloader code is disassembled from the two code load segments in the accepted ESP image (entry taken from its header). Its rodata segment is excluded. A synthetic ELF carries the exact application ISA attributes so GNU does not incorrectly treat Zcmp encodings as D/RV64 instructions.',
              'ROM/application use executable ELF sections, including any embedded tables/padding. Candidate traversal stops at unknown/data boundaries; linear inventories are conservative and may include non-code.',
              'XespLoop is counted separately when CSR numbers 0x7c6..0x7cb occur (pinned IDF components/riscv/include/riscv/csr_hwlp.h:26-31). This proves save/restore/configuration sites, not an active hardware-loop body.', '']
    (a.out/'m1a-isa.md').write_text('\n'.join(lines))
    print(json.dumps({s['name']: {'linear':s['total'], 'candidate':s['candidate'], 'unresolved':s['unresolved_count']} for s in stages}, sort_keys=True))

if __name__ == '__main__': main()

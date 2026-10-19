#!/usr/bin/env python3
"""Read ROM only, twice, via esptool's ROM loader (no downloaded stub).
Caller must reserve the hardware port on the board before running this script.
Usage: IDF-python s31/dump-rom-data.py PORT LOCAL_NOTES_OUTPUT_DIRECTORY
"""
import hashlib
import importlib.metadata
import json
from pathlib import Path
import subprocess
import sys

START = 0x2F83F700
SIZE = 0x8900  # Half-open end 0x2f848000, the next public ELF PT_LOAD.


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    if importlib.metadata.version('esptool') != '5.6.0':
        raise SystemExit('requires esptool 5.6.0')
    port, directory = sys.argv[1:]
    out = Path(directory).resolve()
    out.mkdir(parents=True, exist_ok=True)
    records = []
    for n in (1, 2):
        dest = out / f'rom-data-{n}.bin'
        cmd = [sys.executable, '-m', 'esptool', '--chip', 'esp32s31',
               '--port', port, '--no-stub', '--before', 'default-reset',
               '--after', 'no-reset', 'dump-mem', hex(START), hex(SIZE), str(dest)]
        # --no-stub prevents RAM uploads. dump-mem uses ROM READ_REG only;
        # loader entry resets the chip but does not write flash or eFuse.
        result = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                timeout=600)
        # Do not persist chip identifiers or port/host paths from esptool output.
        if result.returncode:
            text = result.stdout.decode(errors='replace')
            errors = [s for s in text.splitlines() if 'error' in s.lower()]
            raise SystemExit('ROM read failed: ' + '\n'.join(errors))
        data = dest.read_bytes()
        if len(data) != SIZE:
            raise SystemExit('short ROM dump')
        records.append({'file': dest.name, 'bytes': len(data),
                        'sha256': hashlib.sha256(data).hexdigest()})
    if (out / records[0]['file']).read_bytes() != (out / records[1]['file']).read_bytes():
        raise SystemExit('ROM dumps differ')
    manifest = {'esptool': '5.6.0', 'python': sys.version.split()[0],
                'start': hex(START), 'size': hex(SIZE), 'end_exclusive': hex(START + SIZE),
                'command': 'python -m esptool --chip esp32s31 --port PORT --no-stub --before default-reset --after no-reset dump-mem 0x2f83f700 0x8900 OUTPUT',
                'identical': True, 'dumps': records}
    (out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps(manifest, indent=2))


if __name__ == '__main__':
    main()

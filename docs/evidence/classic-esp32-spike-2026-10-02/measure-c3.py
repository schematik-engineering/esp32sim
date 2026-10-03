import hashlib, json, resource, subprocess, sys, time
from pathlib import Path
root, rom, before, after, output = map(Path, sys.argv[1:])
rows = []
for pair in range(3):
    for label, binary in [('before', before), ('after', after)]:
        args = [str(binary), '--rom', str(rom), '--board', 'none', '--boot', 'rom', '--no-dump', '--max-seconds', '30',
                '--bootloader', 'web/wasm/fw/public/c3-hello-bootloader.bin', '--ptable', 'web/wasm/fw/public/c3-hello-ptable.bin', '--app', 'web/wasm/fw/public/c3-hello_world.bin']
        start = resource.getrusage(resource.RUSAGE_CHILDREN)
        wall = time.monotonic()
        result = subprocess.run(args, cwd=root, capture_output=True, check=True)
        end = resource.getrusage(resource.RUSAGE_CHILDREN)
        import re
        count = int(re.search(r'stop:.*?— (\d+) insns', result.stderr.decode())[1])
        rows.append(dict(pair=pair+1, revision=label, cpu_seconds=end.ru_utime+end.ru_stime-start.ru_utime-start.ru_stime,
                         wall_seconds=time.monotonic()-wall, instructions=count, stdout_sha256=hashlib.sha256(result.stdout).hexdigest()))
output.write_text(json.dumps({'samples': rows, 'binaries': {label: hashlib.sha256(binary.read_bytes()).hexdigest() for label,binary in [('before', before), ('after', after)]}}, indent=2)+'\n')

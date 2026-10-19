# S31 M1c: read-only ROM data supplement

Base: `7f2e475adb435388f6f3fdb8dc6b4752e58d73fc`. Local/fork-only bounded M1 slice, not completed native-ROM-to-hello support.

## Input recipe and provenance

The pinned public rev0 ELF (esp-rom-elfs 20260528, SHA256
`a52ad7513deb656a910a5740125f1cce2c7941f11ce57213b7b43aea93d5ab87`)
has no file bytes in [0x2f83f700,0x2f848000). Its executable PT_LOAD ends at
the former address and rodata PT_LOAD begins at the latter. The public IDF
9a97f6c54ec638111ce55cd36581b3c192f15207 soc.h places both in mask ROM.

After reserving the board's port, with esptool 5.6.0 and Python 3.14.7:

    IDF-python s31/dump-rom-data.py PORT LOCAL_NOTES_OUTPUT_DIRECTORY

Each of two invocations expands to:

    python -m esptool --chip esp32s31 --port PORT --no-stub --before default-reset --after no-reset dump-mem 0x2f83f700 0x8900 OUTPUT

The script pins the esptool version, checks exact length and byte equality,
and records hashes/version/range/command in manifest.json. esptool dump_mem
uses read_reg in four-byte increments. --no-stub avoids downloaded RAM code;
no flash/eFuse operation is invoked. Entering the ROM loader resets the chip;
--after no-reset leaves it there, and esptool closes the port on exit.

On 2026-10-19, both reads returned exactly 35072 bytes, SHA256
`45376d77d7c7d0e9c3359a0d6eb2aa1e369aecb181fee32abc16f93b431c002a`.
Dumps remain local in the S31 notes, not Git; redistribution rights are not
asserted. No identifiers or raw esptool discovery output are retained.

## Execution and limits

Pass the dump explicitly with --rom-data FILE. The CLI reports an actionable
error if that file is absent. Omitting the option intentionally preserves the
M1b missing-data stop. The loader requires the public ELF first, accepts only
the exact fixed range, rejects overwrites and wrong lengths before mutation,
and leaves ROM guest-read-only. Reloading an ELF removes its supplement.
It does not authenticate arbitrary user files; reproduce-m1c.sh authenticates
both pinned inputs before running.

    S31_ROM=PUBLIC_ELF S31_ROM_DATA=LOCAL_DUMP CARGO_TARGET_DIR=BUILD_DIR bash s31/reproduce-m1c.sh

Two independent runs are byte-identical:

    [s31] chip=esp32s31 pc=0x2f83e838 opcode=0xbdc79073 retired=6331 stop=Csr(3036)

The former failing load at PC 0x2f801812 now retires. Stop is an explicitly
unsupported CSR 0xbdc, not an instruction skipped or guessed. No ROM banner,
flash boot, second-stage or app entry is claimed. No new MMIO register, ISA
instruction, CSR semantics or shared RV32/C3/C6 source is added. This slice
stops at the next unaudited vendor CSR rather than inventing semantics.

## Tests and mutations

12 ordinary S31 unit tests pass; two explicit external tests pass (old missing
supplement stop and new supplemented stop). Seven Python ISA-audit tests pass.
The loader unit test uses an explicitly synthetic pattern solely to check all
35072 loaded bytes and both neighbors, exact size, ELF prerequisite,
non-overwrite, readonly behavior and replacement on ELF reload. No synthetic
bytes are used in native-ROM execution.

    python3 s31/mutate_rom_data.py --output OUTPUT/data
    python3 s31/mutate_registers.py --output OUTPUT/registers
    python3 s31/mutate_map.py --output OUTPUT/map

Six new loader mutations (wrong start/end, absent presence, missing copy,
allowed overwrite, missing ELF prerequisite) are killed by assertions in
rom_data_exact_range_readonly_and_atomic_validation. All nine inherited
register/CSR and four map mutations are assertion-killed too. Each harness
checks baseline, restores exact source bytes in finally, and checks restored
baseline. No new ISA or register means no additional per-instruction or MMIO
mapping cases are needed in this slice.

## Full gates

The exact-SHA handoff retains commands, exit statuses and complete logs for
retire-common native/wasm Clippy, ROM-enabled and plain release workspace tests,
both empty-HOME variants, wasm build/eight demos, privacy, and isolated non-LTO
RV32/C3/C6 disassembly vs the base. No existing golden changes are intended.
No JIT source changes: wasm-jit-test.sh is not applicable. CPU comparison:
PENDING (no CPU benchmark, new S31 loading does not enter existing chip paths).

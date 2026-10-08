# EX218: opt-in GPIO waveform decoding

Base: upstream main `017af524`. Related EX205 supplies the existing GPIO pin
transport. EX218 adds a WS2812 decoder and instruction-sized scheduling for boards
that opt in. It uses `gpio_output_at` and the existing GRB decoder.

## Contract and quantum-1 evidence

`BoardModel::uses_gpio_waveform` defaults to false. For an attached waveform board,
`Machine::run` uses unmodeled quantum 1, overriding timing models for this run;
`run_until_cycle` limits each budget to one instruction. The configured quantum
is preserved. Browser external blocks are refused for waveform boards.

The shared `tests/gpio_waveform.rs` program sends three GRB pixels and checks RGB
`[7,11,13]`, `[17,19,23]`, `[29,31,37]`, 145 ordered edges, and each of the 72 high
pulse widths against its transmitted bit, plus 800 ns inter-bit low periods. S3 widths are 96/192 cycles at 240 MHz;
C3/C6 widths are 64/128 cycles at 160 MHz. Setting quantum 1 alone passes these checks with main's CPU/JIT/bus execution
paths and `gpio_output_at`. Native AArch64 execution with JIT enabled retired compiled
instructions. WASM with JIT enabled passed through its existing interpreter
fallback at this budget (zero compiled modules in the focused quantum-1 run).
No JIT helper, store, instruction-position or bus-clock changes are needed.

The final shared regression runs requested quanta 1, 64, 256 and 1024, S3 cores
0 and 1 with JIT enabled/disabled, C3 and C6. Requested quantum 1024 on core 0
uses `run_until_cycle`; the other cases use `run`. It also checks restoration,
the browser guard, and ordinary quantum 64 with `NoBoard`. The CLI test and the
WASM JIT suite execute the same source. Native compiled-instruction assertions
apply only on AArch64 Linux/macOS, where that backend is available.

## Idle path and integration

The CPU, native/WASM JIT, bus structs, GPIO write paths and device ticks are
byte-identical to main. The scheduler checks board opt-in at public execution
entry points. No per-store, per-instruction or per-helper work is added. Structural check:

```sh
git diff 017af524 -- emu-core/src riscv-rv32/src xtensa-lx7/src esp32s3/src/bus.rs esp32c3/src/bus.rs esp32c6/src/bus.rs
```

Result: empty output.

External board models opt in, construct `Ws2812Chain::new(n).with_gpio_clock(hz)`,
feed `gpio_output_at` into `gpio_drive`, publish `gpio_deadline` through
`next_deadline`, and call `advance_gpio` from `advance_to`. The shared CLI/WASM
regression is a concrete board adapter; no built-in board enables this decoder.
The frequency is stored once on the chain. Existing GPIO drive masks supply
output/enable transitions; there is no additional pin-routing mechanism.

## Inputs and verification

`inputs.json` pins the synthetic program source and fetched ROMs. Assets are
obtained with `tools/fetch-demo-assets.sh --no-linux`. No external firmware build
is required. `checks.json` records final command exit statuses with Rust 1.99.0.
Both workspace test invocations start with empty HOME; the ignored-inclusive
run sets only ESP32SIM_ROM_DIR among emulator variables, and the plain run sets
none. CARGO_HOME and RUSTUP_HOME retain access to the pinned toolchain.
Existing goldens are unchanged. `tools/wasm-jit-test.sh` runs the full differential
suite and the shared waveform suite in separate Node processes, bounding retained
generated-module metadata. Node v22.23.1 uses its default heap; no heap override
is part of the required check. The final script passes 111,125 differential cases (99,677 compiled modules
released) and 24 waveform cases (738 compiled modules released). The baseline
differential suite at `017af524` has the same 111,125/99,677 counts.

`mutations.json` records each mutation, exact edit, command and killing result.
The 14 retained rules cover pulse windows, reset deadline, output enable, bounded
storage, invalid frames, repeated levels, nonzero clock, both scheduling entry
points, quantum restoration, browser bypass and opt-in. Each mutation is applied
alone and restored. Deleted position-plumbing and duplicate route rules have no
remaining implementation to mutate. Every recorded mutation is killed.

## Limits

High pulses 150–1100 ns, a 550 ns bit split, minimum 150 ns low and 50 us reset
are decoder policy for WS2812-class signals, not hardware calibration. Maximum
non-reset low width is unconstrained. Invalid pulses discard a frame; incomplete
pixels do not update it. Storage is bounded by strip size. Simultaneous waveform
writers on both S3 cores and physical matrix inversion are not modeled by this
adapter. Quantum 1 trades execution throughput for exact instruction-cycle GPIO
timing only while opted in. No CPU benchmark was run.

## CPU comparison

PENDING

## Evidence privacy

Only public source paths, input hashes, commands and correctness results are
retained. No raw captures, local filesystem identities or session logs are used.

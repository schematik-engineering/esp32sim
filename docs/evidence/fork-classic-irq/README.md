# Classic IRQ fast path on the integration pin

Base: `cb2049f7a3b679cdaf94ee7153f0eda7b1889e63`.

Maintain GPIO level and route summaries on pad/configuration changes, sample the
other live sources once per presentation, and request presentation only when a
source or routing/input state can change. MMIO writes remain conservative
invalidations. The bus-aware reset hook invalidates after the actual scheduler
reset, including guest release during the preceding round; it does not rely on a
write-round latch that can be consumed before reset.

This is a backport, not an upstream-main repin. Keep the base's clock conversion,
GPIO routing, RTC-domain reboot policy and I2C behavior. On this base TIMG, LEDC
and RMT are advanced by Dispatch (APB divider 3); its existing source comparisons
provide the event bit. Do not import the newer manually clocked adapter path.
The LEDC probes therefore advance 48,000 CPU cycles for 16,000 APB ticks; the RTC
watchdog probe advances two slow ticks. RTC analog-restore probes for a hook not
present on this base are omitted; MMIO takeover/release and reboot still run.

## Verification

Use Rust 1.99.0, isolated target directories, and `nice -n 10` on cargo commands.

- Native Clippy: `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`.
- WASM Clippy: `cargo +1.99.0 clippy --release --target wasm32-unknown-unknown
  -p esp32sim-wasm --features jit-tests -- -D warnings`.
- Empty HOME, only ROM path configured: `cargo +1.99.0 test --release --workspace
  -- --include-ignored --skip external_`: 913 passed, none failed or ignored.
- Empty HOME, ROM path unset: `cargo +1.99.0 test --release --workspace`:
  886 passed, none failed, 45 ignored.
- `cargo +1.99.0 test [--release] -p esp32 irq_events -- --nocapture`:
  thirteen probes pass in debug and release (seven bus, six Machine tests).
- `cargo +1.99.0 test --release -p esp32 gpio_irq -- --nocapture`: six pass.
- `RUSTUP_TOOLCHAIN=1.99.0 sh tools/wasm-build.sh`, then
  `node tools/wasm-test.mjs hello c3-hello c6-hello c6-energy-scan c6-contiki
  c6-contiki-net c6-rpl-net panel`: eight demos pass.
- `node tools/check-evidence-privacy.mjs` and `git diff --check`: pass.
- Existing golden files are byte-identical to base; none regenerated.
- No JIT source changed; the conditional JIT-change suite is not applicable.

The bundled scripts accept caller-owned worktrees and targets. Compilation
failures are not mutation kills:

```
python3 docs/evidence/fork-classic-irq/mutate-events.py WORKTREE TARGET OUTPUT
python3 docs/evidence/fork-classic-irq/mutate-gpio.py --workspace WORKTREE --target TARGET --output OUTPUT
```

Eight event mutations and 24 GPIO mutations are assertion-killed. Event mutations
cover idle gating, bus/Dispatch propagation, routed-GPIO detection, post-reset
invalidation, both scheduler reset sites, and board/reset attachment. GPIO cases
cover cached status, type/route removal, software-pending type zero, both cores
and NMI, high/low banks, unchanged output work, and each of seven invalidation
sites. The upstream manual timer/LEDC/RMT mutations do not apply: those devices
remain Dispatch-clocked here and their transition probes kill Dispatch removal.

## Static code generation

```
cargo +1.99.0 build --release --bins
python3 docs/evidence/fork-classic-irq/codegen.py BASE_CLI CANDIDATE_CLI OUTPUT
```

EX230 address normalization preserves opcodes, registers, stack and field offsets.
All 51 selected S3/C3/C6 hot functions match base. Classic selects 20 functions,
four matching; the changed query helpers are inlined/split by LTO, GPIO writes
update the new summaries, tick propagates Dispatch and GPIO transitions, and the
scheduler's reset branch stores the existing dirty bit. Static instruction
counts: tick 2,011 to 2,290; run_unmodeled 1,579 to 1,580; run_modeled 3,445 to
3,429; present_irqs 207 to 234; after_round 81 to 85; step_core 838 to 840.
Unchanged fetch/read32/deadline instruction bodies can differ in call target
identity when LTO changes helper boundaries. This is not a CPU measurement.

## CPU comparison

PENDING. Central measurement and exact-fork-tip independent review are required
before publication. No worker timing or speedup claim.

Known overlap: classic bus/peripheral tick sites also change in the idle adapter
work. That separate optimization is not imported by this backport.

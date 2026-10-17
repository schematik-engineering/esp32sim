# Host-hook composition on classic camera lifecycle

Base: `7352bc817954a6e5e287d5b2118c36315635c659`.

This local composition adds two previously prepared host hooks, not a second
source bank, DMA implementation, camera path or scheduler:

1. Routed GPIO waveform notifications and `PinRoutes::software_output`.
   A raw GPIO-latch callback cannot observe mux,
   route or enable-only changes. Keep attachment/reset notifications and the
   opt-in guard on all four chip buses. The board method is appended after the
   camera methods so their vtable positions stay fixed.
2. `I2s::rx_source()`. This reports the source chosen by
   the last receive interval; the read-only source bank cannot attribute that
   choice. Store slot + 1 in the existing lazy receiver allocation and clear it
   on reset/fallback, without changing hot I2s or bus layouts.

Clients should use the existing `SocBus::attach_pcm_sources` before mutating or
pushing source slots, and retain `SocBus::pcm_sources` for read-only inspection.
C6's controller is under `periph.optional.i2s0`. No mutable `pcm_sources` alias
or public `rx_source` field is reintroduced.

## Reproduction

Use Rust 1.99.0 and the standard checks in CONTRIBUTING.md, including both
Clippy targets, ROM and plain release workspace tests, WASM build/eight demos
and evidence privacy. The targeted tests include `routed_waveform` in both
esp-soc and the CLI, `pcm_sources`, classic `host_devices`, and the existing
camera fixture/lifecycle tests. No golden outputs are changed.

Rule-removal scripts (each restores its source):

    python3 docs/evidence/pcm-selected-source/mutate.py pcm-mutations.json
    python3 docs/evidence/routed-gpio-waveform/mutate.py --out waveform-mutations

The scripts distinguish assertion failures from compilation errors. They
exercise source attribution, reset, fallback, inversion and all four buses'
waveform write/reset/opt-out delivery.

## CPU comparison

PENDING. No benchmark is performed for this local compatibility prerequisite.
No per-tick hook polling, new hot fields or scheduling path is introduced.

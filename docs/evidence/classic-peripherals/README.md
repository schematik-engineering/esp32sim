# EX224: classic ESP32 peripheral adapters

Base: `esp32-classic-2-core`, `024c7c7`. Adds I2C0/1, sixteen LEDC
channels, SPI2/3 and eight RMT channels. Board callbacks are gated while no board is attached;
LED output uses the existing BoardModel/Ws2812Chain path. I2S RX belongs
to EX229. No MCPWM implementation is added for classic in this part.

The shared I2C engine is specialized by command count. Existing chips retain
exactly eight slots and no runtime layout field; classic has sixteen slots
and the old opcode mapping. Main's pin-address device matching is preserved.
S3's checked DMA reader and bounded descriptor visits move into esp-periph;
classic reuses both, with memory-only buffer/descriptor access. The finite
chain traversal is shared with S3 crypto, including cycle rejection. The S3
camera/crypto scatter implementation remains in place. DMA errors do not
publish classic TX completion or send malformed transfers to the board.

No existing chip gains fields or per-tick work. The moved descriptor helpers
are inline and reached only by active DMA. Classic peripheral clocks and
transaction timings are inferred. Hardware fade, RMT receive and bit-level
SPI timing are not modeled. RMT emits bits through the shared decoder and
BoardModel path; streams beyond 4096 bits are bounded and not published as
complete frames. There is no second pulse observer, FIFO mode or speculative
RX ownership error model. Continuous output retains only the latest lap.

Classic LEDC maps HS/LS banks onto two shared Ledc groups with an Esp32 layout.
The inline source hook selects APB, REF_TICK or RC_FAST per classic timer;
existing chips use a constant-source closure, with no extra timer-source lookup.
SPI receive words use the shared fill helper. I2C uses I2c<16> directly, with
blocked-line timeout handled at START. Pin routes use the shared IO_MUX table.
The inactive-board gate has an assertion test that rejects tick/MMIO callbacks.

The shared RMT engine generalization is not included: classic tests require
half-symbol deadlines, an idle tick between continuous laps, and wrapping a
borrowed allocation across word 511. The existing shared engine consumes whole
symbols and rejects allocations past the end of RAM. Unifying these contracts
would expand this part into a timing change for S3/C3/C6; retaining the classic
engine preserves both contracts without adding branches to their tick paths.

## Sources

ESP-IDF v5.5.4 `components/soc/esp32/register/soc/`:
`spi_reg.h` lines 361-372, 1285-1334 and 1414-1468 define transfer, link and
interrupt bits. `rmt_reg.h` lines 79-138 define idle, clock and continuous
controls. `ledc_reg.h` lines 1464-1472 and 1666-1674 define timer resolution.
`i2c_reg.h` defines sixteen command slots; `components/hal/esp32/include/hal/i2c_ll.h`
defines their old opcode mapping. `clk_tree_defs.h:40` supplies the nominal
8.5 MHz RC_FAST clock; the port corrects the prototype's 8 MHz value. Code comments cite these sources. The
firmware boot regression uses the IDF 5.5.4 fixture and hashes from EX223.
No hardware or IDF 4.4 validation is claimed.

## Checks

Use the full EX223 command set on this branch, including both Clippy targets,
empty-HOME workspace runs with ROM-only inputs and with no firmware variables,
all eight production WASM scenarios, and evidence privacy. No goldens are
regenerated. No JIT implementation changes.

[Mutation table](mutations.json) records 27 mutations killed by assertion
failures. Apply each single replacement and run
`cargo +1.99.0 test -p CRATE --lib TEST`, or `--test TARGET TEST` for
rows naming an integration target, then restore it. The LEDC shadow test
isolates channel updates from timer updates; neither missing latch gate is
hidden by the other one.

Results on Rust 1.99.0: both Clippy checks pass with warnings denied;
696 CI-mode workspace tests pass; 674 plain workspace tests pass with 36
ignored. Both workspace runs use an empty HOME. All eight production WASM
scenarios and evidence privacy pass. Goldens remain byte-identical.

## CPU comparison

PENDING

## Privacy and limits

Only source expressions, test names and outcomes are retained. No private
firmware, captures, machine identifiers or timing samples are included.

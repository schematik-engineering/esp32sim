# EX227: classic ADC, DAC and touch pads

Base: `esp32-classic-5-wifi`, `73d4568c`. Builds on EX226. Reuses main's
AnalogInputs, raw-count validation, sample observations and waveform timing.
Classic supplies two SAR register layouts, eighteen ADC pads, two DACs,
ten touch pads and RTC mux isolation. The shared script accepts
`touchpad <gpio> <0|1>` separately from board-panel `touch`.

No conversion work is added to tick. Conversion timestamps are captured only
on SENS writes. New state is appended to classic structs. The shared analog
conversion method becomes public rather than adding another sampler.

## Sources and limits

ESP-IDF v5.5.4 `components/soc/esp32/register/soc/sens_reg.h:473-516`
defines ADC pad/start/done controls; lines 708-739 define touch start controls.
`rtc_io_reg.h:777-795` defines DAC data/power/mux, and `rtc_io_periph.c`
defines RTC pad mapping. `include/soc/adc_channel.h` supplies ADC pin mapping.
`components/esp_adc/esp32/adc_cali_line_fitting.c:84-87` supplies nominal Vref
coefficients. The fixture uses IDF 5.5.4; IDF 4.4 is not validated here.

The voltage model inverts the nominal line-fitting coefficients with a
1100 mV Vref. It does not implement chip-specific two-point calibration or
high-range LUT correction. Touch counts (300 touched, 1000 released),
conversion completion and nominal DAC millivolts are inferred. No hardware
calibration, ADC DMA/continuous mode, DAC cosine generation or touch IRQ
model is claimed. Host analog and touch state survive reset; registers reset.

## Verification

Run the full [EX223 check set](../classic-core/README.md), including empty-HOME
workspace runs and all eight WASM scenarios. Inherited fixture/ROM input
hashes are recorded there. Existing goldens remain unchanged; no JIT changes.

[Mutation table](mutations.json) contains 23 killed replacements covering
start edges, controller selection, pad cardinality, power, width/attenuation,
DAC gates, touch routing, timestamp sampling, RTC GPIO isolation, reset and
script validation. Apply each row and run
`cargo +1.99.0 test -p CRATE --lib TEST`, then restore it.

Results on Rust 1.99.0: both required Clippy checks pass; 727 CI-mode
workspace tests and 705 plain tests (36 ignored) pass with empty HOME.
All eight WASM scenarios and evidence privacy pass. Goldens unchanged.

## CPU comparison

PENDING

## Overlap and privacy

Open PRs #196 and #197 also touch the shared script file, for BLE connection
control and cycle limits respectively; neither provides capacitive touch-pad
input. No shared analog duplicate was found. Evidence contains public source
expressions and test outcomes, without private captures or machine identifiers.

The inline `sens_oneshot` helper shares classic/S3 START, DONE and DATA handling
without tick work; each caller retains its own low-START DATA behavior.
Pass-through RTC_IO and touch-timer accessors are removed.

Review limit: the optional Calibration variant is not adopted. Classic uses a
direct rounded line-fit inverse, while the shared API searches a forward
millivolt curve. Supporting both directions would add code and rounding rules,
so this would not meet the review's equal-size condition. Calibration remains
local and its existing raw-code assertions remain unchanged.

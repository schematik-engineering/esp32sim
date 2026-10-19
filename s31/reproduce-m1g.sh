#!/bin/bash
# Pinned-input native ROM run, including stack-canary loads and two-stop comparison.
set -euo pipefail
cd "$(dirname "$0")/.."
bash s31/reproduce-m1c.sh
printf '%s\n' 'Entropy: shared deterministic Rng, now=low32(retired before access), gated by clock/reset/sampling.'
printf '%s\n' 'Two CRC_DATA reads feed the ROM stack guard normally; no console bytes. Stop: PMU_IMM_PAD_HOLD_ALL.'

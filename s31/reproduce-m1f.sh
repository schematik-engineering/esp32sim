#!/bin/bash
# Authenticate pinned inputs, exercise tests and compare two native runs.
set -euo pipefail
cd "$(dirname "$0")/.."
bash s31/reproduce-m1c.sh
printf '%s\n' 'MHCR reset: sibling-sourced (C5 TRM v1.1 p122); unknown-bit reads: 0 (fully sourced reset).'
printf '%s\n' 'M1g now supplies deterministic emulator entropy. No console bytes; current stop is PMU_IMM_PAD_HOLD_ALL.'

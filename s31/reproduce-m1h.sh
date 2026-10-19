#!/bin/bash
# Pinned-input native ROM execution; no device access or console injection.
set -euo pipefail
cd "$(dirname "$0")/.."
bash s31/reproduce-m1c.sh
printf '%s\n' 'PMU LP/HP pad holds released, eFuse TPGM configured, XTAL clock/default divider read.'
printf '%s\n' 'No console bytes; current stop: write8 CLIC IE[21] at 0x10801055.'

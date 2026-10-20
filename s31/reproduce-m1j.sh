#!/bin/bash
# Pinned native ROM inputs; no device access or synthetic output.
set -euo pipefail
cd "$(dirname "$0")/.."
bash s31/reproduce-m1c.sh
printf '%s\n' 'IO MUX pad configuration stored, not routed / no electrical effect.'
printf '%s\n' 'No console bytes; stop: UART0 REG_UPDATE read32 0x2038a098 (R/W/SC synchronization needs a decision).'

// node external_wasm.mjs MODULE BUILD_DIR ROM_ELF > result.json
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { createJitHost } from '../../../web/wasm/jit.mjs';
const [modulePath, build, rom] = process.argv.slice(2);
assert(modulePath && build && rom, 'needs MODULE, unchanged Server BUILD_DIR and C3 rev3 ROM_ELF');
const moduleBytes = readFileSync(modulePath);
const logs = [];
let w;
const decoder = new TextDecoder();
const mem = () => new Uint8Array(w.memory.buffer);
const jit = createJitHost(() => w);
const { instance } = await WebAssembly.instantiate(moduleBytes, { env: {
  ...jit.imports, host_profile_now: () => performance.now(),
  host_log: (p, n) => logs.push(decoder.decode(mem().subarray(p, p + n))),
} });
w = instance.exports;
const bytes = (data, f) => {
  const p = w.esp32sim_alloc(data.length);
  mem().set(data, p);
  try { return f(p, data.length); } finally { w.esp32sim_free(p, data.length); }
};
const emu = bytes(new TextEncoder().encode('c3'), (p, n) => w.esp32sim_new(p, n, 4, 0));
assert(emu);
assert.equal(w.esp32sim_ble_scan(emu, 1), 1);
assert.equal(w.esp32sim_ble_full(emu), 0);
assert.equal(w.esp32sim_ble_scan(emu, 1), 0);
for (const [kind, path] of [[0, rom], [1, join(build, 'Server.ino.bootloader.bin')],
  [2, join(build, 'Server.ino.partitions.bin')], [3, join(build, 'Server.ino.bin')]]) {
  assert.equal(bytes(readFileSync(path), (p, n) => w.esp32sim_load(emu, kind, p, n)), 0);
}
assert.equal(w.esp32sim_boot(emu, 0), 0);
assert.equal(w.esp32sim_ble_full(emu), 1);
const observations = [];
const command = text => bytes(new TextEncoder().encode(text), (p, n) => w.esp32sim_ble_command(emu, p, n));
assert.equal(command('discover'), 1);
assert.equal(w.esp32sim_ble_central_stop(emu), 1);
function runUntil(cycles) {
  while (w.esp32sim_cycles(emu) < cycles) {
    assert.equal(w.esp32sim_run(emu, 2_000_000, 0), 0);
    for (;;) {
      const len = w.esp32sim_ble_take(emu);
      if (!len) break;
      const ptr = w.esp32sim_ble_ptr(emu);
      observations.push(decoder.decode(mem().subarray(ptr, ptr + len)));
    }
  }
}
runUntil(128_000_000);
const responses = observations.filter(s => s.includes('type=SCAN_RSP'));
assert(responses.length >= 15);
assert(responses.every(s => s.includes('name="BLE Server Example"')));
assert.equal(w.esp32sim_ble_scan(emu, 0), 0);
assert.equal(command('connect'), 0);
runUntil(160_000_000);
assert(observations.some(s => s.startsWith('[ble-state] connected')));
assert.equal(command('read-uuid 4fafc201-1fb5-459e-8fcc-c5c9c331914b beb5483e-36e1-4688-b7f5-ea07361b26a8'), 0);
runUntil(536_000_000);
const read = observations.find(s => s.includes('[ble-att] value=') && s.includes('text="Hello World says Neil"'));
assert(read);
assert.equal(w.esp32sim_ble_central_stop(emu), 0);
runUntil(960_000_000);
const resumed = observations.find(s => s.includes('disconnected advertising_resumed'));
assert(resumed);
const time = s => Number(/hus=(\d+)/.exec(s)[1]);
const data = observations.filter(s => s.startsWith('[ble-central]') && s.includes('type=DATA'));
assert(time(data.at(-1)) - time(data[0]) >= 4_000_000);
assert(time(resumed) - time(data.at(-1)) >= 3_900_000);
assert(time(resumed) - time(data.at(-1)) <= 4_200_000);
const events = observations.filter(s => s.startsWith('[ble-connection]'));
for (let i = 0; i < events.length; i++) {
  assert.equal(Number(/channel=(\d+)/.exec(events[i])[1]), (i + 1) * 5 % 37);
}
assert(!observations.some(s => s.startsWith('[ble-error]') || s.startsWith('[ble-observer]')));
assert(!logs.some(s => /assert|panic|exception/i.test(s)));
w.esp32sim_delete(emu);
console.log(JSON.stringify({
  checks: 'pass', module_sha256: createHash('sha256').update(moduleBytes).digest('hex'),
  modeled_seconds: 6, scan_responses: responses.length, sample: responses[0],
  connection_events: events.length, first_event: events[0], last_event: events.at(-1),
  central_pdus: data.length, exchange_duration_us: (time(data.at(-1)) - time(data[0])) / 2,
  read, resumed, guest_errors: logs.filter(s => s.includes('ble_uuid_flat')),
}, null, 2));

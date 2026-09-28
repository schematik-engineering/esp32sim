#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createJitHost } from "../web/wasm/jit.mjs";
const [firmwareRoot, romRoot, ...chips] = process.argv.slice(2);
assert(
  firmwareRoot && romRoot,
  "Usage: node tools/wifi-wasm-test.mjs <Wi-Fi firmware root> <ROM directory>",
);
const bytes = readFileSync(
  new URL(
    "../target/wasm32-unknown-unknown/release/esp32sim_wasm.wasm",
    import.meta.url,
  ),
);
const enc = new TextEncoder(),
  dec = new TextDecoder();
let failed = false;
for (const chip of chips.length ? chips : ["esp32c3", "esp32c6"]) {
  let w, emu;
  const logs = [],
    streams = new Map();
  const host = createJitHost(() => w);
  const { instance } = await WebAssembly.instantiate(bytes, {
    env: {
      ...host.imports,
      host_log: (p, n) => logs.push(dec.decode(mem().subarray(p, p + n))),
    },
  });
  w = instance.exports;
  const mem = () => new Uint8Array(w.memory.buffer);
  const put = (bytes, fn) => {
    const p = w.esp32sim_alloc(bytes.length);
    try {
      mem().set(bytes, p);
      return fn(p, bytes.length);
    } finally {
      w.esp32sim_free(p, bytes.length);
    }
  };
  const until = (predicate) => {
    const deadline = Date.now() + 120000,
      target = w.esp32sim_cycles(emu) + w.esp32sim_cpu_hz(emu) * 30;
    while (!predicate()) {
      assert(
        Date.now() < deadline && w.esp32sim_cycles(emu) < target,
        "Wi-Fi scan/connect/DHCP deadline exceeded",
      );
      assert.equal(w.esp32sim_run(emu, 1000000, Date.now()), 0);
      for (let i = 0, n = w.esp32sim_out_take(emu); i < n; i++)
        if (w.esp32sim_out_kind(emu, i) === 1) {
          const p = w.esp32sim_out_ptr(emu, i),
            n = w.esp32sim_out_len(emu, i),
            m = JSON.parse(dec.decode(mem().subarray(p, p + n)));
          if (m.t === "serial")
            streams.set(
              m.src,
              ((streams.get(m.src) || "") + m.data).slice(-4096),
            );
        }
    }
  };
  try {
    const a = JSON.parse(
      readFileSync(join(firmwareRoot, chip, "artifact.json")),
    );
    emu = put(enc.encode(chip === "esp32s3" ? "none" : chip), (p, n) =>
      w.esp32sim_new(p, n, a.flashBytes / 1048576, a.psramBytes / 1048576),
    );
    const rom = {
      esp32s3: "esp32s3_rev0_rom.elf",
      esp32c3: "esp32c3_rev3_rom.elf",
      esp32c6: "esp32c6_rev0_rom.elf",
    }[chip];
    assert.equal(
      put(readFileSync(join(romRoot, rom)), (p, n) =>
        w.esp32sim_load(emu, 0, p, n),
      ),
      0,
    );
    for (const file of a.artifacts)
      assert.equal(
        put(Buffer.from(file.data, "base64"), (p, n) =>
          w.esp32sim_load_at(emu, file.offset, p, n),
        ),
        0,
      );
    assert.equal(
      put(enc.encode("esp32sim"), (p, n) =>
        put(enc.encode("esp32sim-pass"), (q, m) =>
          w.esp32sim_wifi_configure(emu, p, n, q, m, 6),
        ),
      ),
      0,
    );
    assert.equal(w.esp32sim_wifi_state(emu), 0);
    assert.equal(w.esp32sim_boot(emu, 0), 0);
    until(() =>
      [...streams.values()].some((s) => s.includes("WIFI:IP:10.0.2.15")),
    );
    assert([...streams.values()].some((s) => s.includes("WIFI:AP_FOUND")));
    assert.equal(w.esp32sim_wifi_state(emu), 2);
    console.log(`PASS ${chip}: scan + WPA2 + DHCP through real WASM`);
  } catch (error) {
    failed = true;
    console.error(
      `FAIL ${chip}: ${error.message}\n${[...streams].map(([s, t]) => s + ":" + t).join("\n")}\n${logs.slice(-5).join("\n")}`,
    );
  } finally {
    if (emu) w.esp32sim_delete(emu);
  }
}
if (failed) process.exitCode = 1;

#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createJitHost } from "../web/wasm/jit.mjs";
const [firmwareRoot, romRoot] = process.argv.slice(2);
assert(
  firmwareRoot && romRoot,
  "Usage: node tools/adc-test.mjs <IO firmware root> <ROM directory>",
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
for (const chip of ["esp32s3", "esp32c3", "esp32c6"]) {
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
    const deadline = Date.now() + 20000,
      target = w.esp32sim_cycles(emu) + w.esp32sim_cpu_hz(emu) * 3;
    while (!predicate()) {
      assert(
        Date.now() < deadline && w.esp32sim_cycles(emu) < target,
        "ADC fixture deadline exceeded",
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
      ),
      f = JSON.parse(readFileSync(join(firmwareRoot, chip, "fixture.json")));
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
    assert.equal(w.esp32sim_boot(emu, 0), 0);
    until(() => [...streams.values()].some((s) => s.includes("IO:READY")));
    const source = [...streams.entries()].find(([, s]) =>
      s.includes("IO:READY"),
    )[0];
    assert.notEqual(w.esp32sim_set_adc(emu, 99, 1024), 0);
    assert.notEqual(w.esp32sim_set_adc(emu, f.pins.SIM_ADC_PIN, 4096), 0);
    for (const value of [0, 1024, 3072, 4095]) {
      streams.set(source, "");
      assert.equal(w.esp32sim_set_adc(emu, f.pins.SIM_ADC_PIN, value), 0);
      put(
        enc.encode(JSON.stringify({ t: "key", src: source, data: "A" })),
        (p, n) => w.esp32sim_in_text(emu, p, n),
      );
      until(() => (streams.get(source) || "").includes(`IO:ADC:${value}\r\n`));
    }
    console.log(
      `PASS ${chip}: raw ADC 0,1024,3072,4095 on GPIO${f.pins.SIM_ADC_PIN}`,
    );
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

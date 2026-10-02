import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [upstream, firmwareRoot, romRoot, wasmPath, jpegPath] = process.argv.slice(2);
assert(
  upstream && firmwareRoot && romRoot && wasmPath && jpegPath,
  "Usage: node external-camera.mjs <upstream checkout> <firmware root> <rom root> <add-on wasm> <jpeg>",
);
const { createJitHost } = await import(
  pathToFileURL(resolve(upstream, "web/wasm/jit.mjs"))
);
const wasm = readFileSync(
  wasmPath,
);
const enc = new TextEncoder(),
  dec = new TextDecoder();
for (const sensor of [0x26, 0x5640]) {
  const chip = "esp32s3";
  let w, emu, serial;
  const streams = new Map(),
    logs = [];
  const host = createJitHost(() => w);
  const { instance } = await WebAssembly.instantiate(wasm, {
    env: {
      ...host.imports,
      host_log: (p, n) => logs.push(dec.decode(mem().subarray(p, p + n))),
    },
  });
  w = instance.exports;
  assert(w.__indirect_function_table instanceof WebAssembly.Table, "Build the add-on with its build.rs table exports");
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
  const run = (cycles = 1000000) => {
    assert.equal(w.esp32sim_run(emu, cycles, Date.now()), 0);
    for (let i = 0, n = w.esp32sim_out_take(emu); i < n; i++) {
      if (w.esp32sim_out_kind(emu, i) !== 1) continue;
      const p = w.esp32sim_out_ptr(emu, i),
        length = w.esp32sim_out_len(emu, i);
      const message = JSON.parse(dec.decode(mem().subarray(p, p + length)));
      if (message.t === "serial")
        streams.set(
          message.src,
          ((streams.get(message.src) || "") + message.data).slice(-8192),
        );
    }
  };
  const until = (predicate) => {
    const deadline = Date.now() + 60000;
    const end = w.esp32sim_cycles(emu) + w.esp32sim_cpu_hz(emu) * 8;
    while (!predicate()) {
      assert(
        Date.now() < deadline && w.esp32sim_cycles(emu) < end,
        `Camera deadline exceeded: ${[...streams.values()].join("\n")}\n${logs.slice(-5).join("\n")}`,
      );
      run();
    }
  };
  const command = (data, marker) => {
    streams.set(serial, "");
    put(enc.encode(JSON.stringify({ t: "key", src: serial, data })), (p, n) =>
      w.esp32sim_in_text(emu, p, n),
    );
    until(
      () =>
        (streams.get(serial) || "").includes(marker) &&
        (streams.get(serial) || "").endsWith("\n"),
    );
    return streams.get(serial).replaceAll("\r", "");
  };
  try {
    const artifact = JSON.parse(
      readFileSync(join(firmwareRoot, chip, "artifact.json")),
    );
    const fixture = JSON.parse(
      readFileSync(join(firmwareRoot, chip, "fixture.json")),
    );
    const capture = JSON.parse(
      readFileSync(
        join(firmwareRoot, chip, "flash-artifacts.json"),
      ),
    );
    for (const file of artifact.artifacts) {
      const original = capture.files.find(
        (c) => c.filename === file.filename && c.offset === file.offset,
      );
      assert(
        original,
        "Firmware segment must come from the compiler flash manifest",
      );
      assert.equal(
        createHash("sha256")
          .update(Buffer.from(file.data, "base64"))
          .digest("hex"),
        original.sha256,
        "Firmware bytes must match the compiler output exactly",
      );
    }
    emu = put(enc.encode(chip === "esp32s3" ? "none" : chip), (p, n) =>
      w.esp32sim_new(
        p,
        n,
        artifact.flashBytes / 1048576,
        artifact.psramBytes / 1048576,
      ),
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
    for (const file of artifact.artifacts)
      assert.equal(
        put(Buffer.from(file.data, "base64"), (p, n) =>
          w.esp32sim_load_at(emu, file.offset, p, n),
        ),
        0,
      );
    const c = fixture.camera;
    c.sensor = sensor;
    const config = Buffer.from([
      c.sensor & 255,
      c.sensor >> 8,
      c.id,
      c.sda,
      c.scl,
      c.xclk,
      c.pclk,
      c.vsync,
      c.href,
      ...c.data,
      c.pwdn,
      c.reset,
      c.fps,
    ]);
    assert.equal(
      put(config, (p, n) => w.esp32sim_camera_configure(emu, p, n)),
      0,
    );
    assert.equal(w.esp32sim_boot(emu, 0), 0);
    until(() => [...streams.values()].some((s) => s.includes("CAMERA:READY:")));
    serial = [...streams.entries()].find(([, s]) =>
      s.includes("CAMERA:READY:"),
    )[0];
    assert.deepEqual(
      [0, 1, 2, 3].map((f) => w.esp32sim_camera_info(emu, 0, f)),
      [96, 96, 0, 1],
    );
    assert(
      streams.get(serial).includes(`CAMERA:READY:0:${c.sensor.toString(16)}`),
    );
    assert(
      command("C", "CAMERA:EMPTY").includes("CAMERA:EMPTY"),
      "Missing host input must not produce a blank frame",
    );
    const frame = Buffer.alloc(96 * 96 * 2);
    for (let i = 0; i < frame.length; i += 2)
      frame.writeUInt16LE((i * 37) & 65535, i);
    assert.equal(
      put(frame, (p, n) => w.esp32sim_camera_push(emu, 0, 96, 96, 0, p, n)),
      0,
    );
    for (let capture = 0; capture < 3; capture++) {
      const rgbLine = command("C", "CAMERA:FRAME:");
      assert(
        rgbLine.includes("CAMERA:FRAME:96:96:0:18432:19e161b5:00:b6"),
        rgbLine,
      );
      console.log(`sensor=${sensor.toString(16)} capture=${capture} ${rgbLine.trim()}`);
    }
    assert(
      command("B", "CAMERA:READY:").includes(
        `CAMERA:READY:0:${sensor.toString(16)}`,
      ),
    );
    assert(
      command("C", "CAMERA:FRAME:").includes(
        "CAMERA:FRAME:96:96:0:18432:19e161b5:00:b6",
      ),
      "Host frame survives firmware reboot",
    );
    assert(
      command("J", "CAMERA:READY:").includes(
        `CAMERA:READY:0:${sensor.toString(16)}`,
      ),
    );
    assert.deepEqual(
      [0, 1, 2, 3].map((f) => w.esp32sim_camera_info(emu, 0, f)),
      [160, 120, 3, 1],
    );
    const jpeg = readFileSync(jpegPath);
    assert.equal(
      put(jpeg, (p, n) => w.esp32sim_camera_push(emu, 0, 160, 120, 3, p, n)),
      0,
    );
    for (let capture = 0; capture < 3; capture++) {
      const line = command("C", "CAMERA:FRAME:");
      assert(line.includes("CAMERA:FRAME:160:120:4:2443:0ead8274:ff:d9"), line);
      console.log(`sensor=${sensor.toString(16)} after-deinit-init=${capture} ${line.trim()}`);
    }
    assert.equal(
      put(jpeg, (p, n) => w.esp32sim_camera_push(emu, 1, 160, 120, 3, p, n)),
      1,
    );
    assert.equal(
      put(jpeg, (p, n) => w.esp32sim_camera_push(emu, 0, 96, 96, 3, p, n)),
      2,
    );
    assert.equal(
      put(frame, (p, n) => w.esp32sim_camera_push(emu, 0, 1601, 1200, 0, p, n)),
      2,
    );
    assert.equal(w.esp32sim_camera_reset(emu, 0), 0);
    // The firmware may have already queued one captured framebuffer before host reset.
    command("C", "CAMERA:");
    assert(command("C", "CAMERA:EMPTY").includes("CAMERA:EMPTY"));
    assert(host.stats.compiled > 0);
    assert.equal(host.stats.failed, 0);
    console.log(`sensor=${sensor.toString(16)} jit-compiled=${host.stats.compiled} jit-failed=${host.stats.failed}`);
    console.log(
      `S3 sensor ${sensor.toString(16)}: exact RGB565 and JPEG DMA, reboot, empty input, identity and bounds passed`,
    );
  } finally {
    if (emu) w.esp32sim_delete(emu);
  }
}

#!/usr/bin/env node
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
const [firmwareRoot, romRoot, ...chips] = process.argv.slice(2);
assert(
  firmwareRoot && romRoot,
  "Usage: node tools/wifi-test.mjs <firmware root> <ROM root> [chip...]",
);
const roms = {
  esp32s3: "esp32s3_rev0_rom.elf",
  esp32c3: "esp32c3_rev3_rom.elf",
  esp32c6: "esp32c6_rev0_rom.elf",
};
let failed = false;
for (const chip of chips.length ? chips : ["esp32c3", "esp32c6"]) {
  const dir = join(firmwareRoot, chip),
    artifact = JSON.parse(readFileSync(join(dir, "artifact.json")));
  const output = join(dir, "wifi-run");
  mkdirSync(output, { recursive: true });
  const args = [
    "--chip",
    chip.replace("esp32", ""),
    "--board",
    "none",
    "--boot",
    "rom",
    "--rom",
    join(romRoot, roms[chip]),
    "--console",
    "uart0",
    "--elf",
    join(dir, ".pio/build/serial/firmware.elf"),
    "--max-seconds",
    process.env.ESP32SIM_WIFI_SECONDS || "30",
    "--profile",
    "--log-periph",
  ];
  for (const file of artifact.artifacts) {
    const path = join(output, file.filename);
    writeFileSync(path, Buffer.from(file.data, "base64"));
    args.push("--flash-at", file.offset.toString(16) + "=" + path);
  }
  args.push("--wifi", "ssid=esp32sim,psk=esp32sim-pass", "--net", "none");
  const result = spawnSync(
    fileURLToPath(new URL("../target/release/esp32sim", import.meta.url)),
    args,
    { encoding: "utf8", timeout: 120000, maxBuffer: 4 * 1024 * 1024 },
  );
  writeFileSync(join(output, "serial.log"), result.stdout || "");
  writeFileSync(join(output, "diagnostics.log"), result.stderr || "");
  const pass =
    result.status === 0 &&
    result.stdout.includes("WIFI:AP_FOUND") &&
    result.stdout.includes("WIFI:IP:10.0.2.15");
  failed ||= !pass;
  console.log(
    `${pass ? "PASS" : "FAIL"} ${chip}: scan + WPA2 + DHCP; diagnostics ${output}`,
  );
  console.log((result.stdout || "").slice(-3000));
}
if (failed) process.exitCode = 1;

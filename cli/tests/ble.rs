use std::{path::PathBuf, process::Command};

#[test]
#[ignore = "set ESP32SIM_BLE_FIRMWARE_DIR to the Arduino builds and ESP32SIM_ROM_DIR to the ROM directory"]
fn external_ble_arduino_examples() {
    let firmware = PathBuf::from(std::env::var_os("ESP32SIM_BLE_FIRMWARE_DIR")
        .expect("set ESP32SIM_BLE_FIRMWARE_DIR to the Arduino builds: CHIP/EXAMPLE/.pio/build/BOARD/firmware.{elf,factory.bin}"));
    let roms = PathBuf::from(std::env::var_os("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to the S3/C3/C6 ROM ELF directory"));
    for (chip, board, rom) in [("s3", "esp32-s3-devkitc-1", "esp32s3_rev0_rom.elf"),
                              ("c3", "esp32-c3-devkitm-1", "esp32c3_rev3_rom.elf"),
                              ("c6", "esp32-c6-devkitc-1", "esp32c6_rev0_rom.elf")] {
        assert!(roms.join(rom).is_file(), "missing ROM input {}", roms.join(rom).display());
        for example in ["Server", "Notify", "Write", "Scan"] {
            for name in ["firmware.elf", "firmware.factory.bin"] {
                let file = firmware.join(chip).join(example).join(".pio/build").join(board).join(name);
                assert!(file.is_file(), "missing Arduino BLE input {}", file.display());
            }
        }
    }
    let output = std::env::temp_dir().join(format!("esp32sim-ble-{}", std::process::id()));
    let result = Command::new("python3").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/evidence/ble-s3-c3-c6-2026-10-02/run.py"))
        .args(["--emulator", env!("CARGO_BIN_EXE_esp32sim"), "--firmware"]).arg(firmware)
        .arg("--roms").arg(roms).arg("--output").arg(&output).output().expect("python3 BLE evidence runner");
    assert!(result.status.success(), "Arduino BLE checks failed; logs in {}\n{}\n{}", output.display(),
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

#[test]
fn ble_script_without_enable_exits_unsuccessfully() {
    let script = std::env::temp_dir().join(format!("esp32sim-ble-invalid-{}.txt", std::process::id()));
    for line in ["0 ble conect", "0 ble connect"] {
        std::fs::write(&script, line).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_esp32sim")).args(["--script", script.to_str().unwrap(), "--max-insns", "1", "--no-dump"])
            .output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("line 1"));
    }
    std::fs::remove_file(script).unwrap();
}

#[test]
fn c6_ble_without_elf_names_the_missing_symbol() {
    let result = Command::new(env!("CARGO_BIN_EXE_esp32sim")).args(["--chip", "c6", "--boot", "app", "--ble", "--max-insns", "1", "--no-dump"])
        .output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires ELF symbol esp_bt_controller_init"));
}

#[test]
fn full_ble_mode_is_c3_only_and_excludes_hci() {
    for args in [vec!["--chip", "s3", "--ble", "full"], vec!["--chip", "c3", "--ble", "--ble", "full"], vec!["--chip", "c3", "--ble", "full", "--ble"]] {
        let result = Command::new(env!("CARGO_BIN_EXE_esp32sim")).args(args).output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("mutually exclusive"));
    }
}

#[test]
#[ignore = "set ESP32SIM_BLE_SERVER_DIR to the unchanged Arduino 3.3.11 C3 Server build and ESP32SIM_ROM_DIR to the ROM directory"]
fn external_full_ble_server_advertises_repeatedly() {
    let build = PathBuf::from(std::env::var_os("ESP32SIM_BLE_SERVER_DIR").expect("set ESP32SIM_BLE_SERVER_DIR to the Arduino C3 Server build"));
    let rom = PathBuf::from(std::env::var_os("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to the C3 rev3 ROM directory")).join("esp32c3_rev3_rom.elf");
    assert!(rom.is_file(), "missing C3 rev3 ROM input");
    let mut command = Command::new(env!("CARGO_BIN_EXE_esp32sim-c3"));
    command.args(["--boot", "rom", "--rom"]).arg(rom).args(["--ble", "full", "--flash-mb", "4", "--max-seconds", "2", "--no-dump", "--irq-latency", "--ble-observe",
        "--trace-fn", "r_sch_prog_ble_push", "--trace-fn", "r_lld_adv_evt_start_cbk", "--trace-fn", "r_sch_prog_end_isr_handler"]);
    for (flag, suffix) in [("--bootloader", "bootloader.bin"), ("--ptable", "partitions.bin"), ("--app", "bin"), ("--elf", "elf")] {
        let file = build.join(format!("Server.ino.{suffix}"));
        assert!(file.is_file(), "missing Arduino Server input {}", file.display());
        command.arg(flag).arg(file);
    }
    let result = command.output().expect("run C3 Server specimen");
    let console = String::from_utf8_lossy(&result.stdout);
    let trace = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{trace}");
    assert!(console.contains("Characteristic defined! Now you can read it in your phone!"), "{console}");
    for expected in ["r_sch_prog_ble_push_hack(a0=", "r_lld_adv_evt_start_cbk(a0=", "sources [8]", "r_sch_prog_end_isr_handler(a0=", "0 exceptions"] {
        assert!(trace.contains(expected), "missing {expected}: {trace}");
    }
    assert!(!console.contains("assert"), "{console}");
    assert!(!trace.contains("[ble-error]"), "{trace}");
    // The unchanged sketch puts the name in the conditional SCAN_RSP descriptor.
    assert!(trace.contains("[ble-config] SCAN_RSP name=\"BLE Server Example\""), "{trace}");
    let packets: Vec<_> = trace.lines().filter(|line| line.starts_with("[ble-air]")).collect();
    assert!(packets.len() >= 15, "{trace}");
    let mut previous = None;
    for event in packets.as_chunks::<3>().0 {
        for (line, channel) in event.iter().zip([37, 38, 39]) {
            assert!(line.contains(&format!("channel={channel} type=ADV_IND AdvA=60:55:f9:00:11:24")), "{line}");
            assert!(line.contains("service=4fafc201-1fb5-459e-8fcc-c5c9c331914b"), "{line}");
            assert!(!line.contains("name="), "{line}");
        }
        let hus: u64 = event[0].split_whitespace().find_map(|v| v.strip_prefix("hus=")).unwrap().parse().unwrap();
        if let Some(prior) = previous {
            // Guest interval at lld_adv_env[0]+100 is 0x60 * 625 us, plus 0..10 ms delay.
            assert!((120_000..=140_000).contains(&(hus - prior)), "interval={} half-us", hus - prior);
        }
        previous = Some(hus);
    }
}

#[test]
#[ignore = "set ESP32SIM_BLE_SERVER_DIR to the unchanged Arduino 3.3.11 C3 Server build and ESP32SIM_ROM_DIR to the ROM directory"]
fn external_full_ble_server_responds_to_active_scan() {
    let build = PathBuf::from(std::env::var_os("ESP32SIM_BLE_SERVER_DIR").expect("set ESP32SIM_BLE_SERVER_DIR to the Arduino C3 Server build"));
    let rom = PathBuf::from(std::env::var_os("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to the C3 rev3 ROM directory")).join("esp32c3_rev3_rom.elf");
    assert!(rom.is_file(), "missing C3 rev3 ROM input");
    let mut command = Command::new(env!("CARGO_BIN_EXE_esp32sim-c3"));
    command.args(["--boot", "rom", "--rom"]).arg(rom).args(["--ble", "full", "--ble-scan", "--ble-observe",
        "--flash-mb", "4", "--max-seconds", "2", "--no-dump", "--trace-fn", "r_lld_rxdesc_free"]);
    for (flag, suffix) in [("--bootloader", "bootloader.bin"), ("--ptable", "partitions.bin"), ("--app", "bin"), ("--elf", "elf")] {
        let file = build.join(format!("Server.ino.{suffix}"));
        assert!(file.is_file(), "missing Arduino Server input {}", file.display());
        command.arg(flag).arg(file);
    }
    let result = command.output().expect("run C3 Server specimen");
    let console = String::from_utf8_lossy(&result.stdout);
    let trace = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{trace}");
    assert!(console.contains("Characteristic defined! Now you can read it in your phone!"), "{console}");
    assert!(!console.contains("assert") && !trace.contains("[ble-error]"), "{console}\n{trace}");
    assert!(trace.contains("0 exceptions"), "{trace}");
    let packets: Vec<_> = trace.lines().filter(|line| line.starts_with("[ble-air]") || line.starts_with("[ble-central]")).collect();
    assert!(packets.len() >= 45 && packets.len().is_multiple_of(9), "{trace}");
    let time = |line: &str| -> u64 { line.split_whitespace().find_map(|v| v.strip_prefix("hus=")).unwrap().parse().unwrap() };
    let mut previous = None;
    for (index, exchange) in packets.as_chunks::<3>().0.iter().enumerate() {
        for line in exchange { assert!(line.contains(&format!("channel={}", 37 + index % 3)), "{line}"); }
        assert!(exchange[0].contains("type=ADV_IND AdvA=60:55:f9:00:11:24"), "{}", exchange[0]);
        assert!(exchange[1].contains("type=SCAN_REQ"), "{}", exchange[1]);
        assert!(exchange[2].contains("type=SCAN_RSP AdvA=60:55:f9:00:11:24 name=\"BLE Server Example\""), "{}", exchange[2]);
        assert_eq!(time(exchange[1]) - time(exchange[0]), 2 * (8 * (35 + 8) + 150));
        assert_eq!(time(exchange[2]) - time(exchange[1]), 2 * (8 * (14 + 8) + 150));
        if index.is_multiple_of(3) {
            let current = time(exchange[0]);
            if let Some(prior) = previous { assert!((120_000..=140_000).contains(&(current - prior))); }
            previous = Some(current);
        }
    }
    // Both the ROM veneer and its body can be traced. At least one free per request
    // proves repeated guest consumption, beyond simply printing configured SCAN_RSP.
    assert!(trace.matches("r_lld_rxdesc_free(a0=").count() >= packets.len() / 3, "{trace}");
}

#[test]
#[ignore = "set ESP32SIM_BLE_SERVER_DIR to the unchanged Arduino 3.3.11 C3 Server build and ESP32SIM_ROM_DIR to the ROM directory"]
fn external_full_ble_server_connects_and_times_out() {
    let build = PathBuf::from(std::env::var_os("ESP32SIM_BLE_SERVER_DIR").expect("set ESP32SIM_BLE_SERVER_DIR to the Arduino C3 Server build"));
    let rom = PathBuf::from(std::env::var_os("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to the C3 rev3 ROM directory")).join("esp32c3_rev3_rom.elf");
    assert!(rom.is_file(), "missing C3 rev3 ROM input");
    let mut command = Command::new(env!("CARGO_BIN_EXE_esp32sim-c3"));
    command.args(["--boot", "rom", "--rom"]).arg(rom).args(["--ble", "full", "--ble-connect",
        "--ble-stop-after-ms", "2500", "--ble-observe", "--flash-mb", "4", "--max-seconds", "6", "--no-dump",
        "--trace-fn", "r_llc_disconnect_end", "--trace-fn", "r_lld_con_tx_isr"]);
    for (flag, suffix) in [("--bootloader", "bootloader.bin"), ("--ptable", "partitions.bin"), ("--app", "bin"), ("--elf", "elf")] {
        let file = build.join(format!("Server.ino.{suffix}"));
        assert!(file.is_file(), "missing Arduino Server input {}", file.display());
        command.arg(flag).arg(file);
    }
    let result = command.output().expect("run C3 Server specimen");
    let console = String::from_utf8_lossy(&result.stdout);
    let trace = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{trace}");
    assert!(!console.contains("assert") && !trace.contains("[ble-error]"), "{console}\n{trace}");
    assert!(trace.contains("0 exceptions"), "{trace}");
    assert!(trace.contains("r_lld_con_tx_isr(a0=0x1"), "{trace}");
    let number = |line: &str, key: &str| -> u64 {
        line.split_whitespace().find_map(|s| s.strip_prefix(key)).unwrap().parse().unwrap()
    };
    let connect = trace.lines().find(|l| l.contains("type=CONNECT_IND")).unwrap();
    let events: Vec<_> = trace.lines().filter(|l| l.starts_with("[ble-connection]")).collect();
    let anchor = number(events[0], "anchor_hus=");
    // CONNECT_IND airtime 352 us, then 1.25 ms + WinOffset(6)*1.25 ms.
    assert_eq!(anchor - number(connect, "hus="), 2 * (352 + 8750));
    for (i, event) in events.iter().enumerate() {
        assert_eq!(number(event, "channel="), ((i as u64 + 1) * 5) % 37);
        assert_eq!(number(event, "anchor_hus="), anchor + i as u64 * 60_000);
    }
    let data: Vec<_> = trace.lines().filter(|l| l.contains("type=DATA")).collect();
    assert!(data.len() >= 140);
    let mut central_sn = 0;
    let mut peripheral_sn = 0;
    let mut last_tx = 0;
    for pair in data.as_chunks::<2>().0 {
        assert!(pair[0].starts_with("[ble-central]") && pair[1].starts_with("[ble-air]"));
        assert_eq!(number(pair[0], "channel="), number(pair[1], "channel="));
        assert_eq!(number(pair[1], "hus=") - number(pair[0], "hus="), 2 * (80 + 150));
        let header = |line: &str| u8::from_str_radix(&line.split("pdu=").nth(1).unwrap()[..2], 16).unwrap();
        let a = header(pair[0]);
        let b = header(pair[1]);
        assert_eq!((a >> 3) & 1, central_sn);
        assert_eq!((a >> 2) & 1, peripheral_sn);
        assert_eq!((b >> 3) & 1, peripheral_sn);
        assert_eq!((b >> 2) & 1, central_sn ^ 1);
        central_sn ^= 1;
        peripheral_sn ^= 1;
        last_tx = number(pair[0], "hus=");
    }
    assert!(last_tx - anchor >= 4_000_000);
    assert!(trace.lines().any(|l| l.contains("r_llc_disconnect_end(a0=0x1") && l.contains("a2=0x8")), "{trace}");
    let resumed = trace.lines().find(|l| l.contains("[ble-state] disconnected advertising_resumed")).unwrap();
    assert!((3_900_000..=4_200_000).contains(&(number(resumed, "hus=") - last_tx)));
    assert!(trace[trace.find(resumed).unwrap()..].contains("type=ADV_IND"));
}

#[test]
#[ignore = "set ESP32SIM_BLE_SERVER_DIR to the unchanged Arduino 3.3.11 C3 Server build and ESP32SIM_ROM_DIR to the ROM directory"]
fn external_full_ble_server_reads_gatt_by_uuid() {
    let build = PathBuf::from(std::env::var_os("ESP32SIM_BLE_SERVER_DIR").expect("set ESP32SIM_BLE_SERVER_DIR to the Arduino C3 Server build"));
    let rom = PathBuf::from(std::env::var_os("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to the C3 rev3 ROM directory")).join("esp32c3_rev3_rom.elf");
    assert!(rom.is_file(), "missing C3 rev3 ROM input");
    let mut command = Command::new(env!("CARGO_BIN_EXE_esp32sim-c3"));
    command.args(["--boot", "rom", "--rom"]).arg(rom).args(["--ble", "full", "--ble-connect",
        "--ble-read-uuid", "4fafc201-1fb5-459e-8fcc-c5c9c331914b", "beb5483e-36e1-4688-b7f5-ea07361b26a8", "--ble-observe", "--flash-mb", "4", "--max-seconds", "3", "--no-dump",
        "--trace-fn", "r_llc_disconnect_end", "--trace-fn", "r_lld_con_tx_isr"]);
    for (flag, suffix) in [("--bootloader", "bootloader.bin"), ("--ptable", "partitions.bin"), ("--app", "bin"), ("--elf", "elf")] {
        let file = build.join(format!("Server.ino.{suffix}"));
        assert!(file.is_file(), "missing Arduino Server input {}", file.display());
        command.arg(flag).arg(file);
    }
    let result = command.output().expect("run C3 Server specimen");
    let console = String::from_utf8_lossy(&result.stdout);
    let trace = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{trace}");
    assert!(!console.contains("assert") && !trace.contains("[ble-error]"), "{console}\n{trace}");
    assert!(trace.contains("0 exceptions"), "{trace}");
    assert!(trace.contains("[ble-att] value=48656c6c6f20576f726c642073617973204e65696c text=\"Hello World says Neil\""), "{trace}");
    assert!(!trace.contains("[ble-state] disconnected"), "{trace}");
    // Assert the packets came from the guest's ATT service and declaration responses.
    assert!(trace.contains("05000400070e001000"), "{trace}");
    assert!(trace.contains("1700040009150f000a1000a8261b3607eaf5b78846e1363e48b5be"), "{trace}");
}

#[test]
#[ignore = "set ESP32SIM_BLE_SERVER_DIR to the unchanged Arduino 3.3.11 C3 Server build and ESP32SIM_ROM_DIR to the ROM directory"]
fn external_full_ble_server_reads_gatt_from_script() {
    let build = PathBuf::from(std::env::var_os("ESP32SIM_BLE_SERVER_DIR").expect("set ESP32SIM_BLE_SERVER_DIR to the Arduino C3 Server build"));
    let rom = PathBuf::from(std::env::var_os("ESP32SIM_ROM_DIR").expect("set ESP32SIM_ROM_DIR to the C3 rev3 ROM directory")).join("esp32c3_rev3_rom.elf");
    assert!(rom.is_file(), "missing C3 rev3 ROM input");
    let mut command = Command::new(env!("CARGO_BIN_EXE_esp32sim-c3"));
    command.args(["--boot", "rom", "--rom"]).arg(rom).args(["--ble", "full",
 "--ble-observe", "--flash-mb", "4", "--max-seconds", "3", "--no-dump",
        "--trace-fn", "r_llc_disconnect_end", "--trace-fn", "r_lld_con_tx_isr"]);
    for (flag, suffix) in [("--bootloader", "bootloader.bin"), ("--ptable", "partitions.bin"), ("--app", "bin"), ("--elf", "elf")] {
        let file = build.join(format!("Server.ino.{suffix}"));
        assert!(file.is_file(), "missing Arduino Server input {}", file.display());
        command.arg(flag).arg(file);
    }
    let script = std::env::temp_dir().join(format!("esp32sim-full-ble-script-{}.txt", std::process::id()));
    std::fs::write(&script, "0.5 ble connect\n0.7 ble read-uuid 4fafc201-1fb5-459e-8fcc-c5c9c331914b beb5483e-36e1-4688-b7f5-ea07361b26a8\n").unwrap();
    command.arg("--script").arg(&script);
    let result = command.output().expect("run C3 Server specimen");
    std::fs::remove_file(script).unwrap();
    let console = String::from_utf8_lossy(&result.stdout);
    let trace = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{trace}");
    assert!(!console.contains("assert") && !trace.contains("[ble-error]"), "{console}\n{trace}");
    assert!(trace.contains("0 exceptions"), "{trace}");
    assert!(trace.contains("[ble-att] value=48656c6c6f20576f726c642073617973204e65696c text=\"Hello World says Neil\""), "{trace}");
    assert!(!trace.contains("[ble-state] disconnected"), "{trace}");
    // Assert the packets came from the guest's ATT service and declaration responses.
    assert!(trace.contains("05000400070e001000"), "{trace}");
    assert!(trace.contains("1700040009150f000a1000a8261b3607eaf5b78846e1363e48b5be"), "{trace}");
}

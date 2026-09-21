# Networking

Goal: firmware that needs the network — the Waveshare autopling web UI / `/api/pling`, the
esp32-screen Home Assistant panel, Atech cloud events — runs in the emulator and talks to
real hosts (Home Assistant, price APIs, a browser on the Mac), with no root privileges.

**Status: done, over emulated 802.11.** Firmware associates with a virtual access point through
the unmodified Espressif blob (docs/wifi-plan.md), and the Ethernet traffic that comes out of the
MAC is handled by two layers in front of the host network:

- `esp-wifi/src/net.rs` — the emulated subnet 10.0.2.0/24 (station 10.0.2.15, gateway 10.0.2.2,
  resolver 10.0.2.3): ARP, DHCP, ICMP echo, DNS and an SNTP server serving the host clock.
- `esp-wifi/src/nat.rs` — `--net nat` (the default): everything addressed past the gateway is
  terminated in the emulator and relayed over ordinary host sockets, which is how Contiki-NG's
  NAT64 does it. A guest SYN becomes a `TcpStream::connect` on a worker thread, guest payload is
  written to that socket, socket reads come back as segments the emulator sequences, acknowledges
  and retransmits; UDP flows are a bound `UdpSocket` per (port, destination) with a reply path
  and an idle reaper. Name lookups are forwarded to the host's own first resolver from
  `/etc/resolv.conf`. `--net none` refuses outbound traffic instead (immediate RST).

No libslirp, no tun device, no entitlement, no root. TLS works: the panel fetches
`https://www.elprisetjustnu.se` and polls Home Assistant on the LAN. That needed the RSA/MPI
accelerator, SHA over GDMA (including SHA-384) and AES-CTR — see docs/peripherals.md.

## Not there yet

- **Inbound**: no port forwarding, so a server in the guest (autopling's web UI) is not reachable
  from the Mac. A `hostfwd=tcp:127.0.0.1:8080-:80` option over the same NAT is the natural next step.
- **Multicast/mDNS**: not carried, so `esp-web.local` and Home Assistant discovery do not resolve;
  use IP addresses.
- **Real LAN presence**: a `--net tap`/vmnet backend would give the guest an address on the real
  network (and mDNS with it), at the cost of root or the macOS vmnet entitlement.

## Alternatives considered

- **libslirp** (QEMU's user-mode NAT) instead of the Rust NAT: a C dependency and an FFI thread for
  behaviour we needed only a subset of. The subset turned out to be a few hundred lines.
- **An `esp_wifi` shim or the OpenCores Ethernet MAC** (`CONFIG_ETH_USE_OPENETH`, which IDF ships a
  driver for) instead of emulating the 802.11 MAC: cheaper, but it needs a firmware config change,
  so binaries would no longer be the ones that run on the board. Emulating the MAC kept "unmodified
  firmware" true; neither route is built.

## Browser Ethernet relay transport

The WASM ABI exposes `esp32sim_net_enable`, `esp32sim_net_rx`,
`esp32sim_net_tx_take`, `esp32sim_net_tx_ptr`, and `esp32sim_net_tx_len`.
Configure the virtual AP before enabling transport. Enabling an unmodeled radio
fails. S3 forwards Ethernet II frames through bounded64-frame queues instead of
its local VirtualNet. Frames are14..1518 bytes; copy output bytes before the next
`take`/`enable`. Keep relay mode enabled when the host connection fails so the
emulator does not silently resume local network responses.

`cargo build --release -p esp32sim --bin esp32sim-relay` builds a separate native
subnet process. Stdin/stdout use a4-byte little-endian length followed by one
Ethernet frame. Input length0 polls sockets. The process never opens a listening
socket; a trusted authenticated WebSocket service owns its lifetime and pipes.
Do not expose these pipes directly to untrusted clients.

This relay uses16 TCP and16 UDP flows, bounded TCP windows, numeric public IPv4
destinations checked immediately before socket creation, and a public DNS
resolver at1.1.1.1. It rejects private/reserved IPv4, all IPv6, fragments and IP
options. DNS answers cannot bypass the destination policy. TCP forwards opaque
bytes, so TLS remains in the guest. UDP replies are accepted only from the
connected remote socket and datagrams exceeding the Ethernet MTU are dropped.
Guest HTTP server injection is not yet implemented by this binary.

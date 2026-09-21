//! User-mode NAT: the guest's TCP and UDP flows are terminated here and relayed through ordinary
//! host sockets — the same trick Contiki-NG's border router plays with NAT64, and what QEMU's slirp
//! does, but without the C dependency. The emulator speaks TCP to the firmware and the host's socket
//! API to the world, so no privileges, no tap device and no routing setup are needed.
//!
//! Deliberately simple: no window scaling, no SACK, no congestion control. Segments are small, the
//! "link" never reorders, and lwIP retransmits anything we drop.

use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{channel, Receiver};

const MSS: usize = 1400;
const WINDOW: u16 = 5840;
const RETRANSMIT_US: u64 = 300_000;
const IDLE_CLOSE_US: u64 = 120_000_000;

fn ip(a: &[u8; 4]) -> Ipv4Addr { Ipv4Addr::new(a[0], a[1], a[2], a[3]) }

#[derive(PartialEq, Debug)]
enum State { Connecting, Established, GuestClosed, HostClosed, Done }

struct Tcp {
    guest_mac: [u8; 6], guest_ip: [u8; 4], guest_port: u16, dst_ip: [u8; 4], dst_port: u16,
    pending: Option<Receiver<std::io::Result<TcpStream>>>,
    sock: Option<TcpStream>,
    state: State,
    our_seq: u32,       // next sequence number we will hand out
    guest_seq: u32,     // next sequence number we expect from the guest
    to_host: Vec<u8>,
    unacked: Vec<u8>,   // sent but not acknowledged, starting at `unacked_seq`
    unacked_seq: u32,
    last_tx_us: u64,
    last_activity_us: u64,
}

struct Udp {
    guest_mac: [u8; 6], guest_ip: [u8; 4], guest_port: u16, dst_ip: [u8; 4], dst_port: u16,
    reply_src: [u8; 4],      // what the guest believes it is talking to (DNS is redirected to the host resolver)
    sock: UdpSocket,
    last_activity_us: u64,
}

pub struct Nat {
    tcp: Vec<Tcp>,
    udp: Vec<Udp>,
    isn: u32,
    restricted: bool,
    pub resolver: [u8; 4],
    pub log: bool,
    pub tcp_opened: u64, pub tcp_refused: u64, pub udp_flows: u64,
    pub bytes_to_host: u64, pub bytes_to_guest: u64,
}

impl Nat {
    pub fn new(log: bool) -> Self {
        Nat { restricted: false, tcp: Vec::new(), udp: Vec::new(), isn: 0x1000, resolver: host_resolver(),
              log,
              tcp_opened: 0, tcp_refused: 0, udp_flows: 0, bytes_to_host: 0, bytes_to_guest: 0 }
    }

    pub fn restricted() -> Self {
        let mut nat = Self::new(false);
        nat.restricted = true;
        nat.resolver = [1, 1, 1, 1];
        nat
    }

    // -------------------------------------------------------------- UDP

    /// Forward a UDP datagram and remember the flow so replies find their way back.
    #[allow(clippy::too_many_arguments, reason = "packet fields stay explicit at the protocol boundary")]
    pub fn udp_out(&mut self, gmac: &[u8; 6], gip: &[u8; 4], sport: u16, dip: &[u8; 4], reply_src: &[u8; 4],
                   dport: u16, payload: &[u8], now_us: u64) {
        if payload.len() > 1472 || dport == 0 || self.restricted && !crate::relay::public_ipv4(ip(dip)) { return; }
        let idx = self.udp.iter().position(|f| f.guest_port == sport && f.dst_ip == *dip && f.dst_port == dport);
        let idx = match idx {
            Some(i) => i,
            None => {
                if self.udp.len() >= crate::relay::MAX_FLOWS { return; }
                let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
                if sock.connect(SocketAddr::new(IpAddr::V4(ip(dip)), dport)).is_err() { return; }
                let _ = sock.set_nonblocking(true);
                self.udp.push(Udp { guest_mac: *gmac, guest_ip: *gip, guest_port: sport, dst_ip: *dip, dst_port: dport, reply_src: *reply_src, sock, last_activity_us: now_us });
                self.udp_flows += 1;
                if self.log { eprintln!("[nat] UDP {}:{} -> {}:{} ({} bytes)", ip(gip), sport, ip(dip), dport, payload.len()); }
                self.udp.len() - 1
            }
        };
        let f = &mut self.udp[idx];
        f.last_activity_us = now_us;
        let _ = f.sock.send(payload);
        self.bytes_to_host += payload.len() as u64;
    }

    // -------------------------------------------------------------- TCP

    /// Handle one TCP segment from the guest; returns frames to send back.
    pub fn tcp_in(&mut self, gmac: &[u8; 6], gip: &[u8; 4], dip: &[u8; 4], seg: &[u8], now_us: u64) -> Vec<Vec<u8>> {
        if seg.len() < 20 { return Vec::new(); }
        let sport = u16::from_be_bytes([seg[0], seg[1]]);
        let dport = u16::from_be_bytes([seg[2], seg[3]]);
        let seq = u32::from_be_bytes([seg[4], seg[5], seg[6], seg[7]]);
        let ack = u32::from_be_bytes([seg[8], seg[9], seg[10], seg[11]]);
        let off = ((seg[12] >> 4) as usize) * 4;
        if off < 20 || off > seg.len() || dport == 0 { return Vec::new(); }
        let flags = seg[13];
        let data = if seg.len() > off { &seg[off..] } else { &[][..] };
        let (syn, fin, rst, is_ack) = (flags & 2 != 0, flags & 1 != 0, flags & 4 != 0, flags & 0x10 != 0);

        let idx = self.tcp.iter().position(|c| c.guest_port == sport && c.dst_port == dport && c.dst_ip == *dip);

        if syn && idx.is_none() {
            if self.tcp.len() >= crate::relay::MAX_FLOWS || self.restricted && !crate::relay::public_ipv4(ip(dip)) { self.tcp_refused += 1; return Vec::new(); }
            let (tx, rx) = channel();
            let addr = SocketAddr::new(IpAddr::V4(ip(dip)), dport);
            std::thread::spawn(move || { let _ = tx.send(TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(4))); });
            self.isn = self.isn.wrapping_add(0x10000);
            self.tcp.push(Tcp { guest_mac: *gmac, guest_ip: *gip, guest_port: sport, dst_ip: *dip, dst_port: dport,
                                pending: Some(rx), sock: None, state: State::Connecting,
                                our_seq: self.isn, guest_seq: seq.wrapping_add(1), to_host: Vec::new(), unacked: Vec::new(),
                                unacked_seq: self.isn, last_tx_us: now_us, last_activity_us: now_us });
            if self.log { eprintln!("[nat] TCP {}:{} -> {}:{} connecting", ip(gip), sport, ip(dip), dport); }
            return Vec::new();                                     // the SYN/ACK waits for the host connect
        }
        let Some(i) = idx else { return Vec::new() };
        let mut out = Vec::new();
        self.tcp[i].last_activity_us = now_us;

        if rst { self.tcp[i].state = State::Done; return out; }
        if is_ack {                                                 // release acknowledged bytes
            let c = &mut self.tcp[i];
            let acked = ack.wrapping_sub(c.unacked_seq) as usize;
            if acked > 0 && acked <= c.unacked.len() { c.unacked.drain(..acked); c.unacked_seq = ack; }
        }
        if !data.is_empty() {
            let c = &mut self.tcp[i];
            if seq == c.guest_seq && c.state == State::Established && c.to_host.len() + data.len() <= WINDOW as usize {
                c.to_host.extend_from_slice(data);
                c.guest_seq = c.guest_seq.wrapping_add(data.len() as u32);
                self.bytes_to_host += data.len() as u64;
            }
            out.push(self.segment(i, 0x10, &[]));                   // ACK (also re-ACKs a retransmit)
        }
        if fin && seq.wrapping_add(data.len() as u32) == self.tcp[i].guest_seq {
            let c = &mut self.tcp[i];
            c.guest_seq = c.guest_seq.wrapping_add(1);
            if c.to_host.is_empty() { if let Some(s) = &c.sock { let _ = s.shutdown(std::net::Shutdown::Write); } }
            c.state = if c.state == State::HostClosed { State::Done } else { State::GuestClosed };
            out.push(self.segment(i, 0x10, &[]));
        }
        out
    }

    /// Pump host sockets: connect results, inbound data, retransmissions, expiry.
    pub fn poll(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for i in 0..self.tcp.len() {
            if self.tcp[i].state == State::Connecting {
                let result = self.tcp[i].pending.as_ref().and_then(|rx| rx.try_recv().ok());
                match result {
                    Some(Ok(sock)) => {
                        let _ = sock.set_nonblocking(true);
                        let _ = sock.set_nodelay(true);
                        let c = &mut self.tcp[i];
                        c.sock = Some(sock); c.pending = None; c.state = State::Established;
                        self.tcp_opened += 1;
                        if self.log { eprintln!("[nat] TCP {}:{} connected", ip(&self.tcp[i].dst_ip), self.tcp[i].dst_port); }
                        out.push(self.segment(i, 0x12, &[]));                  // SYN|ACK
                        self.tcp[i].our_seq = self.tcp[i].our_seq.wrapping_add(1);
                        self.tcp[i].unacked_seq = self.tcp[i].our_seq;
                    }
                    Some(Err(e)) => {
                        self.tcp_refused += 1;
                        if self.log { eprintln!("[nat] TCP {}:{} failed: {}", ip(&self.tcp[i].dst_ip), self.tcp[i].dst_port, e); }
                        out.push(self.segment(i, 0x14, &[]));                  // RST|ACK
                        self.tcp[i].state = State::Done;
                    }
                    None => {}
                }
                continue;
            }
            let c = &mut self.tcp[i];
            if !c.to_host.is_empty() {
                if let Some(sock) = &mut c.sock {
                    match sock.write(&c.to_host) {
                        Ok(n) => { c.to_host.drain(..n); },
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                        Err(_) => c.state = State::Done,
                    }
                    if c.to_host.is_empty() && c.state == State::GuestClosed { let _ = sock.shutdown(std::net::Shutdown::Write); }
                }
            }
            // inbound data
            if matches!(self.tcp[i].state, State::Established | State::GuestClosed) {
                let mut buf = [0u8; MSS];
                loop {
                    if self.unacked_full(i) { break; }
                    let available = (WINDOW as usize - self.tcp[i].unacked.len()).min(MSS);
                    let n = match self.tcp[i].sock.as_mut().map(|s| s.read(&mut buf[..available])) {
                        Some(Ok(0)) => { break_eof(&mut self.tcp[i]); out.push(self.segment(i, 0x11, &[])); self.tcp[i].our_seq = self.tcp[i].our_seq.wrapping_add(1); break; }
                        Some(Ok(n)) => n,
                        Some(Err(ref e)) if e.kind() == ErrorKind::WouldBlock => break,
                        Some(Err(_)) => { self.tcp[i].state = State::Done; break; }
                        None => break,
                    };
                    let payload = buf[..n].to_vec();
                    if self.log { eprintln!("[nat] TCP {}:{} -> guest {} bytes (seq {})", ip(&self.tcp[i].dst_ip), self.tcp[i].dst_port, n, self.tcp[i].our_seq); }
                    out.push(self.segment(i, 0x18, &payload));                  // PSH|ACK
                    let c = &mut self.tcp[i];
                    c.unacked.extend_from_slice(&payload);
                    c.our_seq = c.our_seq.wrapping_add(n as u32);
                    c.last_tx_us = now_us;
                    self.bytes_to_guest += n as u64;
                    if self.unacked_full(i) { break; }
                }
            }
            // retransmit the oldest unacknowledged segment
            let c = &self.tcp[i];
            if !c.unacked.is_empty() && now_us.wrapping_sub(c.last_tx_us) > RETRANSMIT_US {
                let chunk: Vec<u8> = c.unacked.iter().take(MSS).cloned().collect();
                let seq = c.unacked_seq;
                out.push(self.segment_at(i, 0x18, &chunk, seq));
                self.tcp[i].last_tx_us = now_us;
            }
        }
        // UDP replies
        for i in 0..self.udp.len() {
            let mut buf = [0u8; 65535];
            for _ in 0..4 {
                match self.udp[i].sock.recv(&mut buf) {
                    Ok(n) => {
                        if n > 1472 { continue; }
                        if self.log { eprintln!("[nat] UDP reply {} bytes -> guest port {}", n, self.udp[i].guest_port); }
                        self.bytes_to_guest += n as u64;
                        self.udp[i].last_activity_us = now_us;
                        let f = &self.udp[i];
                        out.push(udp_frame(&f.guest_mac, &f.reply_src, &f.guest_ip, f.dst_port, f.guest_port, &buf[..n]));
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }
        self.tcp.retain(|c| c.state != State::Done && now_us.wrapping_sub(c.last_activity_us) < IDLE_CLOSE_US);
        self.udp.retain(|f| now_us.wrapping_sub(f.last_activity_us) < IDLE_CLOSE_US);
        out
    }

    fn unacked_full(&self, i: usize) -> bool { self.tcp[i].unacked.len() >= WINDOW as usize }

    fn segment(&self, i: usize, flags: u8, payload: &[u8]) -> Vec<u8> {
        let seq = self.tcp[i].our_seq;
        self.segment_at(i, flags, payload, seq)
    }

    fn segment_at(&self, i: usize, flags: u8, payload: &[u8], seq: u32) -> Vec<u8> {
        let c = &self.tcp[i];
        let mut t = Vec::with_capacity(20 + payload.len());
        t.extend_from_slice(&c.dst_port.to_be_bytes()); t.extend_from_slice(&c.guest_port.to_be_bytes());
        t.extend_from_slice(&seq.to_be_bytes()); t.extend_from_slice(&c.guest_seq.to_be_bytes());
        t.extend_from_slice(&[0x50, flags]); t.extend_from_slice(&WINDOW.to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]);
        t.extend_from_slice(payload);
        let ck = tcp_checksum(&c.dst_ip, &c.guest_ip, &t);
        t[16..18].copy_from_slice(&ck.to_be_bytes());
        eth_ip(&c.guest_mac, &c.dst_ip, &c.guest_ip, 6, &t)
    }
}

fn break_eof(c: &mut Tcp) { c.state = if c.state == State::GuestClosed { State::Done } else { State::HostClosed }; }

/// The host's first configured resolver, so guest name lookups behave like the host's.
fn host_resolver() -> [u8; 4] {
    if let Ok(conf) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in conf.lines() {
            if let Some(rest) = line.trim().strip_prefix("nameserver ") {
                if let Ok(a) = rest.trim().parse::<Ipv4Addr>() { return a.octets(); }
            }
        }
    }
    [1, 1, 1, 1]
}

fn checksum(data: &[u8], init: u32) -> u16 {
    let mut sum = init;
    let mut i = 0;
    while i + 1 < data.len() { sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32; i += 2; }
    if i < data.len() { sum += (data[i] as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xffff) + (sum >> 16); }
    !(sum as u16)
}

fn tcp_checksum(src: &[u8; 4], dst: &[u8; 4], seg: &[u8]) -> u16 {
    let mut p = Vec::with_capacity(12 + seg.len());
    p.extend_from_slice(src); p.extend_from_slice(dst);
    p.extend_from_slice(&[0, 6]); p.extend_from_slice(&(seg.len() as u16).to_be_bytes());
    p.extend_from_slice(seg);
    checksum(&p, 0)
}

/// Wrap a UDP payload from `src_ip:sport` to the guest.
fn udp_frame(gmac: &[u8; 6], src_ip: &[u8; 4], dst_ip: &[u8; 4], sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let len = 8 + payload.len();
    let mut u = Vec::with_capacity(len);
    u.extend_from_slice(&sport.to_be_bytes()); u.extend_from_slice(&dport.to_be_bytes());
    u.extend_from_slice(&(len as u16).to_be_bytes()); u.extend_from_slice(&[0, 0]);
    u.extend_from_slice(payload);
    let mut p = Vec::with_capacity(12 + len);
    p.extend_from_slice(src_ip); p.extend_from_slice(dst_ip);
    p.extend_from_slice(&[0, 17]); p.extend_from_slice(&(len as u16).to_be_bytes());
    p.extend_from_slice(&u);
    let c = checksum(&p, 0); let c = if c == 0 { 0xffff } else { c };
    u[6..8].copy_from_slice(&c.to_be_bytes());
    eth_ip(gmac, src_ip, dst_ip, 17, &u)
}

/// IPv4 packet inside an Ethernet frame addressed to the guest.
fn eth_ip(gmac: &[u8; 6], src: &[u8; 4], dst: &[u8; 4], proto: u8, payload: &[u8]) -> Vec<u8> {
    let total = 20 + payload.len();
    let mut h = Vec::with_capacity(total);
    h.extend_from_slice(&[0x45, 0x00]); h.extend_from_slice(&(total as u16).to_be_bytes());
    h.extend_from_slice(&[0, 0, 0x40, 0x00, 64, proto, 0, 0]);
    h.extend_from_slice(src); h.extend_from_slice(dst);
    let c = checksum(&h, 0).to_be_bytes(); h[10] = c[0]; h[11] = c[1];
    h.extend_from_slice(payload);
    let mut f = Vec::with_capacity(14 + total);
    f.extend_from_slice(gmac); f.extend_from_slice(&[0x02, 0x53, 0x49, 0x4d, 0x00, 0x02]);
    f.extend_from_slice(&[0x08, 0x00]); f.extend_from_slice(&h);
    f
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    #[test]
    fn restricted_nat_rejects_rebinding_targets_before_opening_sockets() {
        let mut nat = Nat::restricted();
        for dest in [[127,0,0,1], [10,0,0,1], [169,254,169,254], [192,168,1,1]] {
            let mut syn = vec![0;20]; syn[..2].copy_from_slice(&1234u16.to_be_bytes()); syn[2..4].copy_from_slice(&80u16.to_be_bytes()); syn[12]=0x50; syn[13]=2;
            nat.tcp_in(&[2;6], &[10,0,2,15], &dest, &syn, 0);
            nat.udp_out(&[2;6], &[10,0,2,15], 1234, &dest, &dest, 53, b"dns", 0);
        }
        assert!(nat.tcp.is_empty()); assert!(nat.udp.is_empty());
        assert_eq!(nat.tcp_refused,4);
    }

    #[test]
    fn http_binary_response_survives_tcp_windows_and_guest_fin() {
        use std::net::TcpListener;
        use std::time::{Duration, Instant};
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let port=listener.local_addr().unwrap().port();
        let payload:Vec<u8>=(0..18000).map(|n|(n%256) as u8).collect();
        let mut response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",payload.len()).into_bytes(); response.extend_from_slice(&payload);
        let expected=response.clone();
        let server=std::thread::spawn(move || {
            let (mut stream,_)=listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request=Vec::new(); stream.read_to_end(&mut request).unwrap();
            assert_eq!(request,b"GET /binary HTTP/1.1\r\nHost: fixture\r\n\r\n");
            stream.write_all(&response).unwrap();
        });
        let mut nat=Nat::new(false);
        let start=Instant::now();
        let mut seq=100u32;
        let mut ack=0u32;
        let segment=|seq:u32,ack:u32,flags:u8,data:&[u8]| { let mut s=vec![0;20];s[..2].copy_from_slice(&1234u16.to_be_bytes());s[2..4].copy_from_slice(&port.to_be_bytes());s[4..8].copy_from_slice(&seq.to_be_bytes());s[8..12].copy_from_slice(&ack.to_be_bytes());s[12]=0x50;s[13]=flags;s.extend_from_slice(data);s };
        nat.tcp_in(&[2;6], &[10,0,2,15], &[127,0,0,1], &segment(seq,ack,2,&[]), 0);
        seq+=1;
        let mut received=Vec::new();
        let mut sent=false;
        while received.len()<expected.len() {
            assert!(start.elapsed()<Duration::from_secs(5));
            let now=start.elapsed().as_micros() as u64;
            for frame in nat.poll(now) {
                let tcp=&frame[34..];
                let remote_seq=u32::from_be_bytes(tcp[4..8].try_into().unwrap());
                let off=(tcp[12]>>4) as usize*4;
                if tcp[13]&2!=0 {
                    ack=remote_seq.wrapping_add(1);
                    assert!(!sent);sent=true;
                    let request=b"GET /binary HTTP/1.1\r\nHost: fixture\r\n\r\n";
                    nat.tcp_in(&[2;6], &[10,0,2,15], &[127,0,0,1], &segment(seq,ack,0x19,request), now);
                    seq+=request.len() as u32+1;
                } else if !tcp[off..].is_empty() && remote_seq==ack {
                    received.extend_from_slice(&tcp[off..]);ack+=tcp[off..].len() as u32;
                    nat.tcp_in(&[2;6], &[10,0,2,15], &[127,0,0,1], &segment(seq,ack,0x10,&[]),now);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(received,expected);server.join().unwrap();
    }
}

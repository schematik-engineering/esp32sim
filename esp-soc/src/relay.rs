//! Boundary policy for the authenticated browser relay. Host destinations are numeric IPv4
//! addresses checked again by NAT immediately before opening sockets; DNS answers cannot bypass it.
use std::net::Ipv4Addr;

pub const MAX_FRAME: usize = 1518;
pub const MAX_FLOWS: usize = 16;

pub fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0 || a == 10 || a == 127 || a >= 224
        || a == 100 && (64..=127).contains(&b)
        || a == 169 && b == 254 || a == 172 && (16..=31).contains(&b)
        || a == 192 && (b == 168 || b == 0 && c == 0 || b == 0 && c == 2 || b == 88 && c == 99)
        || a == 198 && (b == 18 || b == 19 || b == 51 && c == 100)
        || a == 203 && b == 0 && c == 113)
}

/// IPv6, fragmented IP, options and unsupported protocols fail closed. Guest source addresses
/// are confined to the single station. ARP/DHCP/DNS remain inside this isolated virtual subnet.
pub fn allowed_frame(f: &[u8]) -> bool {
    if !(14..=MAX_FRAME).contains(&f.len()) || f[6] & 1 != 0 { return false; }
    let kind = u16::from_be_bytes([f[12], f[13]]);
    if kind == 0x0806 {
        return f.len() >= 42 && f[14..22] == [0, 1, 8, 0, 6, 4, 0, 1];
    }
    if kind != 0x0800 || f.len() < 34 { return false; }
    let p = &f[14..];
    let total = u16::from_be_bytes([p[2], p[3]]) as usize;
    if p[0] != 0x45 || total < 20 || total > p.len() || u16::from_be_bytes([p[6], p[7]]) & 0x3fff != 0 { return false; }
    let body = &p[20..total];
    let source = &p[12..16];
    let dest = Ipv4Addr::new(p[16], p[17], p[18], p[19]);
    match p[9] {
        17 if body.len() >= 8 => {
            let length = u16::from_be_bytes([body[4], body[5]]) as usize;
            if length != body.len() { return false; }
            let port = u16::from_be_bytes([body[2], body[3]]);
            if port == 67 && body[0..2] == [0, 68] && source == [0, 0, 0, 0] && dest == Ipv4Addr::BROADCAST { return true; }
            source == [10, 0, 2, 15] && port != 0 && (public_ipv4(dest) || dest.octets() == [10, 0, 2, 3] && port == 53)
        }
        6 if body.len() >= 20 => {
            let header = ((body[12] >> 4) as usize) * 4;
            source == [10, 0, 2, 15] && public_ipv4(dest) && body[2..4] != [0, 0] && (20..=body.len()).contains(&header)
        }
        1 => source == [10, 0, 2, 15] && dest.octets() == [10, 0, 2, 2] && body.len() >= 8,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn special_destinations_fail_closed() {
        for ip in ["0.1.2.3", "10.0.0.1", "127.0.0.1", "169.254.169.254", "100.64.0.1", "172.31.1.2", "192.168.1.1", "192.0.0.9", "192.0.2.1", "192.88.99.1", "198.18.1.1", "198.51.100.1", "203.0.113.1", "224.0.0.1", "255.255.255.255"] { assert!(!public_ipv4(ip.parse().unwrap()), "{ip}"); }
        assert!(public_ipv4("1.1.1.1".parse().unwrap()));
        assert!(public_ipv4("93.184.216.34".parse().unwrap()));
        for len in 0..1600 { assert!(!allowed_frame(&vec![0; len])); }
    }
}

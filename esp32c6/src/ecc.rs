use esp_periph::{
    crypto::{bn_mod, bn_modexp, bn_mul},
    Device, WriteEffect,
};

type Number = Vec<u32>;

fn hex(value: &str) -> Number {
    value
        .as_bytes()
        .rchunks(8)
        .map(|part| u32::from_str_radix(std::str::from_utf8(part).unwrap(), 16).unwrap())
        .collect()
}
fn add(a: &[u32], b: &[u32], p: &[u32]) -> Number {
    let mut out = Vec::new();
    let mut carry = 0u64;
    for i in 0..a.len().max(b.len()) {
        let v =
            a.get(i).copied().unwrap_or(0) as u64 + b.get(i).copied().unwrap_or(0) as u64 + carry;
        out.push(v as u32);
        carry = v >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    bn_mod(&out, p)
}
fn sub(a: &[u32], b: &[u32], p: &[u32]) -> Number {
    let a = add(a, &[], p);
    let b = add(b, &[], p);
    let mut out = vec![0; p.len() + 1];
    let mut carry = 0u64;
    for i in 0..p.len() {
        let v = a.get(i).copied().unwrap_or(0) as u64 + p[i] as u64 + carry;
        out[i] = v as u32;
        carry = v >> 32;
    }
    out[p.len()] = carry as u32;
    let mut borrow = 0i64;
    for (i, word) in out.iter_mut().enumerate() {
        let v = *word as i64 - b.get(i).copied().unwrap_or(0) as i64 - borrow;
        *word = v as u32;
        borrow = i64::from(v < 0);
    }
    bn_mod(&out, p)
}
fn mul(a: &[u32], b: &[u32], p: &[u32]) -> Number {
    bn_mod(&bn_mul(a, b), p)
}
fn zero(a: &[u32]) -> bool {
    a.iter().all(|v| *v == 0)
}

struct Curve {
    p: Number,
    b: Number,
}
impl Curve {
    fn new(wide: bool) -> Self {
        if wide {
            Self {
                p: hex("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff"),
                b: hex("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b"),
            }
        } else {
            Self {
                p: hex("fffffffffffffffffffffffffffffffeffffffffffffffff"),
                b: hex("64210519e59c80e70fa7e9ab72243049feb8deecc146b9b1"),
            }
        }
    }
    fn valid(&self, x: &[u32], y: &[u32]) -> bool {
        let p = &self.p;
        // Coordinates must be canonical field elements, not only equal modulo p.
        let mut canonical_x = bn_mod(x, p);
        canonical_x.resize(x.len(), 0);
        let mut canonical_y = bn_mod(y, p);
        canonical_y.resize(y.len(), 0);
        if canonical_x != x || canonical_y != y {
            return false;
        }
        let rhs = add(
            &sub(&mul(&mul(x, x, p), x, p), &mul(x, &[3], p), p),
            &self.b,
            p,
        );
        zero(&sub(&mul(y, y, p), &rhs, p))
    }
}

struct Point {
    x: Number,
    y: Number,
    z: Number,
}
impl Point {
    fn infinity() -> Self {
        Self {
            x: vec![],
            y: vec![],
            z: vec![],
        }
    }
    fn double(&self, p: &[u32]) -> Self {
        if zero(&self.z) || zero(&self.y) {
            return Self::infinity();
        }
        let xx = mul(&self.x, &self.x, p);
        let yy = mul(&self.y, &self.y, p);
        let yyyy = mul(&yy, &yy, p);
        let zz = mul(&self.z, &self.z, p);
        let xy = add(&self.x, &yy, p);
        let s = mul(&sub(&sub(&mul(&xy, &xy, p), &xx, p), &yyyy, p), &[2], p);
        let m = mul(&sub(&xx, &mul(&zz, &zz, p), p), &[3], p);
        let x = sub(&mul(&m, &m, p), &mul(&s, &[2], p), p);
        let y = sub(&mul(&m, &sub(&s, &x, p), p), &mul(&yyyy, &[8], p), p);
        let yz = add(&self.y, &self.z, p);
        let z = sub(&sub(&mul(&yz, &yz, p), &yy, p), &zz, p);
        Self { x, y, z }
    }
    fn add_affine(&self, x: &[u32], y: &[u32], p: &[u32]) -> Self {
        if zero(&self.z) {
            return Self {
                x: x.to_vec(),
                y: y.to_vec(),
                z: vec![1],
            };
        }
        let zz = mul(&self.z, &self.z, p);
        let h = sub(&mul(x, &zz, p), &self.x, p);
        let r = sub(&mul(&mul(y, &self.z, p), &zz, p), &self.y, p);
        if zero(&h) {
            return if zero(&r) {
                self.double(p)
            } else {
                Self::infinity()
            };
        }
        let hh = mul(&h, &h, p);
        let hhh = mul(&h, &hh, p);
        let v = mul(&self.x, &hh, p);
        let nx = sub(&sub(&mul(&r, &r, p), &hhh, p), &mul(&v, &[2], p), p);
        let ny = sub(&mul(&r, &sub(&v, &nx, p), p), &mul(&self.y, &hhh, p), p);
        Self {
            x: nx,
            y: ny,
            z: mul(&self.z, &h, p),
        }
    }
    fn affine(self, p: &[u32]) -> (Number, Number) {
        if zero(&self.z) {
            return (vec![], vec![]);
        }
        let inv = bn_modexp(&self.z, &sub(p, &[2], p), p);
        let inv2 = mul(&inv, &inv, p);
        (
            mul(&self.x, &inv2, p),
            mul(&mul(&self.y, &inv2, p), &inv, p),
        )
    }
}

/// C6 ECC_MULT: little-endian parameter RAM, P-192/P-256 point validation and scalar multiplication.
pub struct Ecc {
    mem: [u32; 24],
    conf: u32,
    raw: u32,
    ena: u32,
}
impl Default for Ecc {
    fn default() -> Self {
        Self {
            mem: [0; 24],
            conf: 1 << 31,
            raw: 0,
            ena: 0,
        }
    }
}
impl Ecc {
    fn calculate(&mut self) {
        let mode = (self.conf >> 5) & 7;
        if !matches!(mode, 0 | 2 | 3) {
            return;
        }
        let curve = Curve::new(self.conf & 4 != 0);
        let n = curve.p.len();
        let x = &self.mem[8..8 + n];
        let y = &self.mem[16..16 + n];
        let valid = curve.valid(x, y);
        self.conf = (self.conf & !(1 | 256)) | if valid { 256 } else { 0 };
        if mode != 2 && (mode == 0 || valid) {
            let mut point = Point::infinity();
            for bit in (0..n * 32).rev() {
                point = point.double(&curve.p);
                if self.mem[bit / 32] >> (bit % 32) & 1 != 0 {
                    point = point.add_affine(x, y, &curve.p);
                }
            }
            let (x, y) = point.affine(&curve.p);
            self.mem[8..].fill(0);
            self.mem[8..8 + x.len()].copy_from_slice(&x);
            self.mem[16..16 + y.len()].copy_from_slice(&y);
        }
        self.raw = 1;
    }
}
impl Device for Ecc {
    fn read(&mut self, off: u32) -> u32 {
        match off {
            0xc => self.raw,
            0x10 => self.raw & self.ena,
            0x14 => self.ena,
            0x1c => self.conf,
            0x100..=0x15c => self.mem[((off - 0x100) / 4) as usize],
            _ => 0,
        }
    }
    fn write(&mut self, off: u32, value: u32) -> WriteEffect {
        match off {
            0x18 => self.raw &= !value,
            0x14 => self.ena = value & 1,
            0x1c => {
                if value & 2 != 0 {
                    *self = Self::default();
                } else {
                    self.conf = (value & !256) | (self.conf & 256);
                    if value & 1 != 0 {
                        self.calculate();
                    }
                }
            }
            0x100..=0x15c => self.mem[((off - 0x100) / 4) as usize] = value,
            _ => {}
        }
        WriteEffect::NONE
    }
    fn irq_sources(&self) -> u64 {
        (self.raw & self.ena) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const G256X: &str = "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296";
    const G256Y: &str = "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
    fn write_number(ecc: &mut Ecc, offset: u32, value: &str) {
        for (i, word) in hex(value).iter().enumerate() {
            ecc.write(offset + i as u32 * 4, *word);
        }
    }
    fn read_number(ecc: &mut Ecc, offset: u32, words: usize) -> Number {
        (0..words)
            .map(|i| ecc.read(offset + i as u32 * 4))
            .collect()
    }
    #[test]
    fn scalar_results_match_independent_openssl_vectors_on_both_curves() {
        for (wide, x, y, key, expected_x, expected_y) in [
            (
                true,
                G256X,
                G256Y,
                "02",
                "7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978",
                "07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1",
            ),
            (
                true,
                G256X,
                G256Y,
                "123456789abcdef",
                "3988322ab9f52c7f11d5d1aa92a2ac0b00275bcad8e934682257323fda672482",
                "855b7389f116c19c0014311c3d57dc02001e3a0ec8bd90c797732034aacd9918",
            ),
            (
                false,
                "188da80eb03090f67cbf20eb43a18800f4ff0afd82ff1012",
                "07192b95ffc8da78631011ed6b24cdd573f977a11e794811",
                "123456789abcdef",
                "f262420ea5f28e5140716def549d276bba81e680facf2ed4",
                "66e6151154abb7387156e93fa6955e643082215f0c1718e2",
            ),
        ] {
            let mut ecc = Ecc::default();
            write_number(&mut ecc, 0x100, key);
            write_number(&mut ecc, 0x120, x);
            write_number(&mut ecc, 0x140, y);
            ecc.write(0x14, 1);
            ecc.write(0x1c, (u32::from(wide) << 2) | (3 << 5) | 1);
            assert_eq!(ecc.read(0xc), 1);
            assert_eq!(ecc.irq_sources(), 1);
            assert_eq!(ecc.read(0x1c) & 257, 256);
            let words = if wide { 8 } else { 6 };
            assert_eq!(read_number(&mut ecc, 0x120, words), hex(expected_x));
            assert_eq!(read_number(&mut ecc, 0x140, words), hex(expected_y));
            ecc.write(0x18, 1);
            assert_eq!(ecc.irq_sources(), 0);
        }
    }
    #[test]
    fn verification_rejects_off_curve_and_noncanonical_points() {
        let mut ecc = Ecc::default();
        write_number(&mut ecc, 0x120, G256X);
        write_number(&mut ecc, 0x140, G256Y);
        ecc.write(0x1c, 0x45);
        assert_eq!(ecc.read(0x1c) & 256, 256);
        ecc.write(0x140, ecc.mem[16] ^ 1);
        ecc.write(0x1c, 0x45);
        assert_eq!(ecc.read(0x1c) & 256, 0);
        assert_eq!(ecc.read(0xc), 1);
        write_number(
            &mut ecc,
            0x120,
            "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
        );
        ecc.write(0x1c, 0x45);
        assert_eq!(ecc.read(0x1c) & 256, 0);
    }
    #[test]
    fn zero_scalar_and_group_order_produce_infinity() {
        for key in [
            "0",
            "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
        ] {
            let mut ecc = Ecc::default();
            write_number(&mut ecc, 0x100, key);
            write_number(&mut ecc, 0x120, G256X);
            write_number(&mut ecc, 0x140, G256Y);
            ecc.write(0x1c, 5);
            assert!(zero(&ecc.mem[8..]));
            assert_eq!(ecc.read(0xc), 1);
        }
    }
    #[test]
    fn unsupported_modes_do_not_report_success_and_reset_clears_interrupts() {
        let mut ecc = Ecc::default();
        ecc.write(0x1c, 0x25);
        assert_eq!(ecc.read(0xc), 0);
        ecc.write(0x14, 1);
        ecc.raw = 1;
        ecc.write(0x1c, 2);
        assert_eq!(ecc.irq_sources(), 0);
        assert_eq!(ecc.read(0x1c), 1 << 31);
    }
}

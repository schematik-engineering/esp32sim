use crate::SpiPins;
use esp_periph::{GpSpi, Gpio};

#[derive(Clone, Copy)]
pub enum SpiLayout {
    S3,
    S3Spi3,
    C3,
    C6,
}

impl SpiLayout {
    pub fn pins(self, gpio: &Gpio, spi: &GpSpi) -> SpiPins {
        let (count, signal_mask, oen, input_select, input_mask, clk, mosi, miso, cs): (
            usize,
            u32,
            u32,
            u32,
            u32,
            u32,
            u32,
            u32,
            [u32; 6],
        ) = match self {
            Self::S3 => (
                49,
                0x1ff,
                1 << 10,
                1 << 7,
                0x3f,
                101,
                103,
                102,
                [110, 111, 112, 113, 114, 115],
            ),
            Self::S3Spi3 => (
                49,
                0x1ff,
                1 << 10,
                1 << 7,
                0x3f,
                66,
                68,
                67,
                [71, 72, 127, u32::MAX, u32::MAX, u32::MAX],
            ),
            Self::C3 => (
                22,
                0xff,
                1 << 9,
                1 << 6,
                0x1f,
                63,
                65,
                64,
                [68, 69, 70, 71, 72, 73],
            ),
            Self::C6 => (
                31,
                0xff,
                1 << 9,
                1 << 7,
                0x3f,
                63,
                65,
                64,
                [68, 101, 102, 103, 104, 105],
            ),
        };
        // Each tuple is (function, clock, MOSI, MISO, CS0), from the chip IO_MUX.
        let native: &[(u32, u8, u8, u8, u8)] = match self {
            Self::S3 => &[(4, 12, 11, 13, 10), (2, 36, 35, 37, 34)],
            Self::S3Spi3 => &[],
            Self::C3 => &[(2, 6, 7, 2, 10)],
            Self::C6 => &[(2, 6, 7, 2, 16)],
        };
        let valid = |pin: usize| {
            pin < count && !(matches!(self, Self::S3 | Self::S3Spi3) && (22..26).contains(&pin))
        };
        let outputs = |signal: u32, role: usize| {
            let mut mask = 0;
            for pin in 0..count {
                if !valid(pin) {
                    continue;
                }
                let function = (gpio.io_mux[pin] >> 12) & 7;
                let sel = gpio.func_out_sel[pin];
                let matrix = function == 1
                    && sel & (signal_mask | (oen >> 1) | (oen << 1)) == signal
                    && (sel & oen == 0 || gpio.enable & (1u64 << pin) != 0);
                let mux = native
                    .iter()
                    .any(|&(f, c, o, _, s)| f == function && [c, o, s][role] as usize == pin);
                if matrix || mux {
                    mask |= 1u64 << pin;
                }
            }
            mask
        };
        let user = spi.read(0x10);
        let mut pins = SpiPins {
            sclk: outputs(clk, 0),
            ..Default::default()
        };
        if user & ((1 << 31) | (1 << 30) | (1 << 27)) != 0 {
            pins.mosi = outputs(mosi, 1);
        }
        for (i, signal) in cs.iter().enumerate() {
            if spi.read(0x20) & (1 << i) == 0 {
                if i == 0 {
                    pins.cs |= outputs(*signal, 2);
                } else {
                    for pin in 0..count {
                        let sel = gpio.func_out_sel[pin];
                        if valid(pin)
                            && (gpio.io_mux[pin] >> 12) & 7 == 1
                            && sel & (signal_mask | (oen >> 1) | (oen << 1)) == *signal
                            && (sel & oen == 0 || gpio.enable & (1u64 << pin) != 0)
                        {
                            pins.cs |= 1u64 << pin;
                        }
                    }
                }
            }
        }
        if user & (1 << 28) != 0 {
            let sel = gpio.func_in_sel[miso as usize];
            if sel & input_select != 0 {
                let pin = (sel & input_mask) as usize;
                if valid(pin)
                    && sel & (input_select >> 1) == 0
                    && gpio.io_mux[pin] & (7 << 12 | 1 << 9) == (1 << 12 | 1 << 9)
                {
                    pins.miso = Some(pin as u8);
                }
            } else {
                pins.miso = native.iter().find_map(|&(f, _, _, pin, _)| {
                    (gpio.io_mux[pin as usize] & (7 << 12 | 1 << 9) == (f << 12 | 1 << 9))
                        .then_some(pin)
                });
            }
        }
        pins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_and_mirrored_matrix_routes_require_matching_mux_and_phase() {
        for (layout, clk, mosi, cs) in [
            (SpiLayout::S3, 101, 103, 110),
            (SpiLayout::C3, 63, 65, 68),
            (SpiLayout::C6, 63, 65, 68),
        ] {
            let mut gpio = Gpio::new();
            let mut spi = GpSpi::new();
            spi.write(0x10, 1 << 27);
            spi.write(0x20, 0x3e);
            for (pin, signal) in [(0, clk), (1, mosi), (3, mosi), (4, cs)] {
                gpio.set_io_mux(pin, 1 << 12);
                gpio.func_out_sel[pin as usize] = signal;
            }
            let pins = layout.pins(&gpio, &spi);
            assert_eq!((pins.sclk, pins.mosi, pins.cs), (1, 2 | 8, 16));
            gpio.set_io_mux(3, 0);
            assert_eq!(layout.pins(&gpio, &spi).mosi, 2);
            spi.write(0x10, 0);
            assert_eq!(layout.pins(&gpio, &spi).mosi, 0);
            spi.write(0x20, 0x3f);
            assert_eq!(layout.pins(&gpio, &spi).cs, 0);
        }
        for (layout, f, c, o, i, s) in [
            (SpiLayout::S3, 4, 12, 11, 13, 10),
            (SpiLayout::C3, 2, 6, 7, 2, 10),
            (SpiLayout::C6, 2, 6, 7, 2, 16),
        ] {
            let mut gpio = Gpio::new();
            let mut spi = GpSpi::new();
            spi.write(0x10, 1 << 27 | 1 << 28);
            spi.write(0x20, 0x3e);
            for pin in [c, o, i, s] {
                gpio.set_io_mux(pin, f << 12 | 1 << 9);
            }
            let pins = layout.pins(&gpio, &spi);
            assert_eq!(
                (pins.sclk, pins.mosi, pins.cs, pins.miso),
                (1 << c, 1 << o, 1 << s, Some(i))
            );
        }
    }
}

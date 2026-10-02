//! C6 physical routes. IO_MUX offsets are 4 + 4 * GPIO number.
use crate::periph::Peripherals;
use esp_soc::board::SpiPins;

impl Peripherals {
    fn mux(&self, pin: usize) -> u32 {
        self.io_mux.read(4 + 4 * pin as u32)
    }

    fn matrix_output(&self, pin: usize, signal: u32) -> bool {
        let sel = self.gpio.func_out_sel[pin];
        self.mux(pin) & (7 << 12) == 1 << 12
            && sel & (0xff | 1 << 8 | 1 << 10) == signal
            && (sel & (1 << 9) == 0 || self.gpio.enable & (1 << pin) != 0)
    }

    pub(crate) fn spi2_pins(&self) -> SpiPins {
        let outputs = |signal, native| {
            let mut mask = 0;
            for pin in 0..31 {
                if self.matrix_output(pin, signal)
                    || (pin == native && self.mux(pin) & (7 << 12) == 2 << 12)
                {
                    mask |= 1 << pin;
                }
            }
            mask
        };
        let user = self.spi2.read(0x10);
        let mut pins = SpiPins {
            sclk: outputs(63, 6),
            ..Default::default()
        };
        if user & (1 << 31 | 1 << 30 | 1 << 27) != 0 {
            pins.mosi = outputs(65, 7);
        }
        for pin in 0..31 {
            if self.matrix_output(pin, 128)
                && self.gpio.enable & (1 << pin) != 0
                && self.gpio.out & (1 << pin) == 0
            {
                pins.cs |= 1 << pin;
            }
            for (cs, signal) in [68, 101, 102, 103, 104, 105].into_iter().enumerate() {
                if self.spi2.read(0x20) & ((1 << cs) | (1 << (cs + 7))) == 0
                    && (self.matrix_output(pin, signal)
                        || (pin == 16 + cs && self.mux(pin) & (7 << 12) == 2 << 12))
                {
                    pins.cs |= 1 << pin;
                }
            }
        }
        if user & (1 << 28) != 0 {
            let sel = self.gpio.func_in_sel[64];
            let pin = (sel & 63) as usize;
            if sel & 0x80 != 0 {
                if pin < 31
                    && sel & 0x40 == 0
                    && self.mux(pin) & (7 << 12 | 1 << 9) == (1 << 12 | 1 << 9)
                {
                    pins.miso = Some(pin as u8);
                }
            } else if self.mux(2) & (7 << 12 | 1 << 9) == (2 << 12 | 1 << 9) {
                pins.miso = Some(2);
            }
        }
        pins
    }
}

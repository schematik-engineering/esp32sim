//! Byte-level UART endpoints for board models. Pins are chip GPIO numbers, so a device's
//! receive pin connects to `tx_pins`, and its transmit pin connects to `rx_pin`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartRoute {
    pub port: usize,
    /// Every non-inverted, enabled TX output, including GPIO matrix fan-out.
    pub tx_pins: u64,
    pub rx_pin: Option<u8>,
    /// None when no source or divider is configured.
    pub baud: Option<u32>,
}
impl UartRoute {
    /// Three percent tolerance for divider rounding and oscillator error.
    pub fn matches_baud(&self, baud: u32) -> bool {
        baud != 0
            && self
                .baud
                .is_some_and(|actual| actual.abs_diff(baud) as u64 * 100 <= baud as u64 * 3)
    }
    pub fn transmits_on(&self, pin: u8) -> bool {
        pin < 64 && self.tx_pins & (1u64 << pin) != 0
    }
}

pub struct UartInput {
    pub pin: u8,
    pub baud: u32,
    /// Completed characters; the ordinary 128-byte RX FIFO/overflow rules apply.
    pub data: Vec<u8>,
}

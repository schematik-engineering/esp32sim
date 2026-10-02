//! One isolated virtual subnet over framed stdin/stdout. No listening sockets or credentials.
use std::io::{self, Read, Write};
use std::time::Instant;
use esp_soc::{nat::Nat, net::VirtualNet, relay::{allowed_frame, MAX_FRAME}};

fn main() -> io::Result<()> {
    let mut net = VirtualNet::new(false);
    net.nat = Some(Nat::restricted());
    let started = Instant::now();
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut header = [0; 4];
        match input.read_exact(&mut header) { Ok(()) => {}, Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()), Err(e) => return Err(e) }
        let len = u32::from_le_bytes(header) as usize;
        if len > MAX_FRAME { return Err(io::Error::new(io::ErrorKind::InvalidData, "frame exceeds MTU")); }
        let now = started.elapsed().as_micros() as u64;
        let mut frame = vec![0; len];
        input.read_exact(&mut frame)?;
        let mut replies = if len > 0 && allowed_frame(&frame) { net.handle(&frame, now) } else { Vec::new() };
        replies.extend(net.poll(now));
        for reply in replies {
            if !(14..=MAX_FRAME).contains(&reply.len()) { continue; }
            output.write_all(&(reply.len() as u32).to_le_bytes())?;
            output.write_all(&reply)?;
        }
        output.flush()?;
    }
}

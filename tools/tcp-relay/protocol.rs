//! Version-two single-stream framing. No target, command or resource crosses it.
use std::io::{self, Read, Write};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
pub const LIMIT: usize = 16 * 1024;
/// Frames a multiplexed stream may have in flight before the receiver returns
/// credit; bounds each stream to WINDOW * LIMIT buffered bytes.
pub const WINDOW: u32 = 64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Data = 1,
    Fin = 2,
    Ping = 3,
    Close = 4,
    Credit = 5,
    Open = 6,
    /// Sent once by a host that knows its peer supports WINDOW credits.
    Window = 7,
}
pub fn read(reader: &mut impl Read, buffer: &mut [u8; LIMIT]) -> io::Result<(Kind, usize)> {
    let mut header = [0; 5];
    reader.read_exact(&mut header)?;
    let kind = match header[0] {
        1 => Kind::Data,
        2 => Kind::Fin,
        3 => Kind::Ping,
        4 => Kind::Close,
        5 => Kind::Credit,
        6 => Kind::Open,
        7 => Kind::Window,
        _ => return Err(io::Error::other("Invalid relay frame")),
    };
    let size = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
    if size > LIMIT || (kind != Kind::Data && size != 0) || (kind == Kind::Data && size == 0) {
        return Err(io::Error::other("Invalid relay frame size"));
    }
    reader.read_exact(&mut buffer[..size])?;
    Ok((kind, size))
}
/// Up to WINDOW payloads can be outstanding per direction. Waiting for a slow
/// TCP peer never occupies the frame parser or its heartbeat path.
#[derive(Default)]
pub struct Credit {
    available: Mutex<u32>,
    changed: Condvar,
}
impl Credit {
    pub fn grant(&self) -> io::Result<()> {
        let mut available = self
            .available
            .lock()
            .map_err(|_| io::Error::other("Credit unavailable"))?;
        if *available >= WINDOW {
            return Err(io::Error::other("Excess relay credit"));
        }
        *available += 1;
        self.changed.notify_one();
        Ok(())
    }
    pub fn take(&self, alive: &AtomicBool) -> io::Result<()> {
        let mut available = self
            .available
            .lock()
            .map_err(|_| io::Error::other("Credit unavailable"))?;
        while *available == 0 {
            if !alive.load(Ordering::Acquire) {
                return Err(io::Error::other("Relay closed"));
            }
            available = self
                .changed
                .wait_timeout(available, Duration::from_millis(100))
                .map_err(|_| io::Error::other("Credit unavailable"))?
                .0;
        }
        *available -= 1;
        Ok(())
    }
}
pub fn write(writer: &mut impl Write, kind: Kind, data: &[u8]) -> io::Result<()> {
    if data.len() > LIMIT
        || (kind != Kind::Data && !data.is_empty())
        || (kind == Kind::Data && data.is_empty())
    {
        return Err(io::Error::other("Invalid relay frame size"));
    }
    let mut header = [0; 5];
    header[0] = kind as u8;
    header[1..].copy_from_slice(&(data.len() as u32).to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(data)?;
    writer.flush()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_is_bounded_and_half_close_is_distinct_from_transport_eof() {
        let mut data = Vec::new();
        write(&mut data, Kind::Data, b"hello").unwrap();
        write(&mut data, Kind::Fin, &[]).unwrap();
        write(&mut data, Kind::Ping, &[]).unwrap();
        let mut input = data.as_slice();
        let mut buffer = [0; LIMIT];
        assert_eq!(read(&mut input, &mut buffer).unwrap(), (Kind::Data, 5));
        assert_eq!(&buffer[..5], b"hello");
        assert_eq!(read(&mut input, &mut buffer).unwrap(), (Kind::Fin, 0));
        assert_eq!(read(&mut input, &mut buffer).unwrap(), (Kind::Ping, 0));
        assert!(read(&mut input, &mut buffer).is_err());
        assert!(write(&mut data, Kind::Data, &vec![0; LIMIT + 1]).is_err());
        assert!(read(&mut [1, 0, 1, 0, 0].as_slice(), &mut buffer).is_err());
        assert!(read(&mut [8, 0, 0, 0, 0].as_slice(), &mut buffer).is_err());
    }
    #[test]
    fn credit_is_bounded_by_the_window_and_never_reused() {
        let credit = Credit::default();
        let alive = AtomicBool::new(true);
        for _ in 0..WINDOW {
            credit.grant().unwrap();
        }
        assert!(credit.grant().is_err(), "a peer cannot grant more than the window");
        for _ in 0..WINDOW {
            credit.take(&alive).unwrap();
        }
        credit.grant().unwrap();
        credit.take(&alive).unwrap();
        alive.store(false, Ordering::Release);
        assert!(credit.take(&alive).is_err());
    }
}

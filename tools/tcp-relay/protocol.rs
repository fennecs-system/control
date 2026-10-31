//! Version-one single-stream framing. No target, command or resource crosses it.
use std::io::{self, Read, Write};
pub const LIMIT: usize = 16 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Data = 1,
    Fin = 2,
    Ping = 3,
    Close = 4,
}
pub fn read(reader: &mut impl Read, buffer: &mut [u8; LIMIT]) -> io::Result<(Kind, usize)> {
    let mut header = [0; 5];
    reader.read_exact(&mut header)?;
    let kind = match header[0] {
        1 => Kind::Data,
        2 => Kind::Fin,
        3 => Kind::Ping,
        4 => Kind::Close,
        _ => return Err(io::Error::other("Invalid relay frame")),
    };
    let size = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
    if size > LIMIT || (kind != Kind::Data && size != 0) || (kind == Kind::Data && size == 0) {
        return Err(io::Error::other("Invalid relay frame size"));
    }
    reader.read_exact(&mut buffer[..size])?;
    Ok((kind, size))
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
        assert!(read(&mut [5, 0, 0, 0, 0].as_slice(), &mut buffer).is_err());
    }
}

//! One stdin/stdout connection to a loopback port inside the container.
use std::{
    io,
    net::{Shutdown, TcpStream},
    time::Duration,
};
fn main() -> io::Result<()> {
    let port = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| io::Error::other("Expected a loopback port"))?;
    let mut socket =
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(5))?;
    let mut outgoing = socket.try_clone()?;
    let _writer = std::thread::spawn(move || {
        let result = io::copy(&mut io::stdin().lock(), &mut outgoing);
        let _ = outgoing.shutdown(Shutdown::Write);
        result
    });
    io::copy(&mut socket, &mut io::stdout().lock())?;
    let _ = socket.shutdown(Shutdown::Read);
    // The HTTP peer closed its response stream. Returning also closes stdout,
    // allowing the host to release a keep-alive client without waiting for stdin.
    Ok(())
}

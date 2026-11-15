//! Bounded blocking HTTP transport. No application state or agent admission.
use std::{
    io::{self, Write},
    net::TcpStream,
    thread::{Builder, JoinHandle},
};

/// Includes long-lived routes; live-update streams have their own limit of 16.
pub(crate) const MAX_HANDLERS: usize = 32;

pub(crate) struct Requests {
    tasks: Vec<JoinHandle<()>>,
    failed: bool,
    overload: Vec<u8>,
}
impl Requests {
    pub fn new() -> Self {
        let mut overload = Vec::new();
        control_http::respond(
            &mut overload,
            "503 Service Unavailable",
            "text/plain",
            b"Control HTTP capacity reached. Retry shortly.\n",
            "Retry-After: 1\r\n",
            control_http::Policy::Page,
        )
        .expect("static overload response");
        Self {
            tasks: Vec::with_capacity(MAX_HANDLERS),
            failed: false,
            overload,
        }
    }

    /// True means admitted. Rejection never reads, waits or creates a thread.
    pub fn dispatch(
        &mut self,
        mut stream: TcpStream,
        handle: impl FnOnce(TcpStream) + Send + 'static,
    ) -> io::Result<bool> {
        let mut index = 0;
        while index < self.tasks.len() {
            if self.tasks[index].is_finished() {
                self.failed |= self.tasks.swap_remove(index).join().is_err();
            } else {
                index += 1;
            }
        }
        if self.tasks.len() == MAX_HANDLERS {
            if stream.set_nonblocking(true).is_ok() {
                // One best-effort write, even if the peer is not reading. A
                // partial write or unavailable send buffer closes the socket.
                let _ = stream.write(&self.overload);
            }
            return Ok(false);
        }
        // Accepted sockets can inherit nonblocking mode on BSD hosts.
        stream.set_nonblocking(false)?;
        let _ = stream.set_nodelay(true);
        self.tasks.push(
            Builder::new()
                .name("control-http".into())
                .spawn(move || handle(stream))?,
        );
        Ok(true)
    }

    pub fn join(mut self) -> io::Result<()> {
        for task in self.tasks {
            self.failed |= task.join().is_err();
        }
        if self.failed {
            Err(io::Error::other(
                "An HTTP handler stopped unexpectedly; inspect node state before restarting.",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;

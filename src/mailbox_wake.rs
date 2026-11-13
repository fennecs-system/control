//! Kernel directory notifications accelerate mailbox delivery. The bounded wait
//! retains lifecycle checks and works even on mounts which lose notifications.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    time::Duration,
};

pub(crate) struct MailboxWake {
    descriptor: Option<OwnedFd>,
    #[cfg(target_os = "macos")]
    _directory: Option<std::fs::File>,
}
impl MailboxWake {
    pub(crate) fn new(directory: &Path) -> Self {
        Self::open(directory).unwrap_or_else(|_| Self {
            descriptor: None,
            #[cfg(target_os = "macos")]
            _directory: None,
        })
    }
    #[cfg(target_os = "macos")]
    fn open(path: &Path) -> io::Result<Self> {
        let directory = std::fs::File::open(path)?;
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let event = libc::kevent {
            ident: directory.as_raw_fd() as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_RENAME | libc::NOTE_DELETE,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe { libc::kevent(fd, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor: Some(descriptor),
            _directory: Some(directory),
        })
    }
    #[cfg(target_os = "linux")]
    fn open(path: &Path) -> io::Result<Self> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = CString::new(path.as_os_str().as_bytes())?;
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe {
            libc::inotify_add_watch(
                fd,
                path.as_ptr(),
                libc::IN_MOVED_TO
                    | libc::IN_CLOSE_WRITE
                    | libc::IN_CREATE
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor: Some(descriptor),
        })
    }
    pub(crate) fn wait(&self, maximum: Duration) {
        let Some(fd) = &self.descriptor else {
            std::thread::sleep(maximum);
            return;
        };
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe {
            libc::poll(
                &mut poll,
                1,
                maximum.as_millis().min(i32::MAX as u128) as i32,
            )
        } <= 0
        {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let _ = unsafe {
                libc::kevent(fd.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &timeout)
            };
        }
        #[cfg(target_os = "linux")]
        {
            let mut buffer = [0u8; 4096];
            // One drain is enough: remaining events cause an immediate next wait.
            let _ = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        }
    }
}

#[cfg(test)]
mod tests;

//! A bounded AF_UNIX connect. A refused/stale socket is down immediately;
//! only genuinely pending or interrupted connects consume the deadline.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

pub(crate) fn connect(path: &Path, until: Instant) -> io::Result<UnixStream> {
    // SAFETY: zero is a valid initial representation of sockaddr_un.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid socket path"));
    }
    addr.sun_family = libc::AF_UNIX as _;
    for (to, from) in addr.sun_path.iter_mut().zip(bytes) {
        *to = *from as _;
    }
    let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        addr.sun_len = len as _;
    }
    loop {
        remaining(until)?;
        // SAFETY: socket has no pointer arguments. Ownership transfers to stream.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a newly created, uniquely owned socket.
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        // Avoid leaking a socket into a concurrent child process.
        // SAFETY: fcntl acts on this valid owned fd.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        stream.set_nonblocking(true)?;
        // SAFETY: addr is initialized, length covers its populated path.
        let rc = unsafe { libc::connect(fd, (&addr as *const libc::sockaddr_un).cast(), len) };
        if rc == 0 {
            stream.set_nonblocking(false)?;
            return Ok(stream);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS) => {
                wait_writable(&stream, until)?;
                if let Some(error) = stream.take_error()? {
                    return Err(error);
                }
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Some(libc::EINTR) | Some(libc::EAGAIN) => {
                // These did not establish a pending connect. Retry using a new
                // socket, with bounded backoff even when poll is immediately ready.
                wait_writable(&stream, until)?;
                if let Some(error) = stream.take_error()? {
                    return Err(error);
                }
                std::thread::sleep(remaining(until)?.min(std::time::Duration::from_millis(1)));
            }
            _ => return Err(error),
        }
    }
}

pub(crate) fn remaining(until: Instant) -> io::Result<std::time::Duration> {
    until
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
}

fn wait_writable(stream: &UnixStream, until: Instant) -> io::Result<()> {
    loop {
        let left = remaining(until)?;
        // Round up to avoid a zero-ms busy loop, saturating poll's signed bound.
        let ms = left.as_millis().saturating_add(u128::from(left.subsec_nanos() % 1_000_000 != 0)).min(i32::MAX as u128)
            as i32;
        let mut fd = libc::pollfd { fd: stream.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
        // SAFETY: one initialized pollfd, valid for the duration of poll.
        let result = unsafe { libc::poll(&mut fd, 1, ms) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

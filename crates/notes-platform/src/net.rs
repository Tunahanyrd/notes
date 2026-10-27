//! The loopback listener (`socketserver.TCPServer` with
//! `allow_reuse_address = False`: no `SO_REUSEADDR`).
//!
//! Linux: `socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC)`. macOS has no
//! `SOCK_CLOEXEC`: `FD_CLOEXEC` is set with `fcntl` right after `socket()`
//! (the engine does not fork between the two calls on that path) and
//! `sockaddr_in` carries `sin_len`.
use std::net::{SocketAddrV4, TcpListener};
use std::os::unix::io::FromRawFd;

#[cfg(not(target_vendor = "apple"))]
unsafe fn stream_socket() -> std::io::Result<i32> {
    let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(target_vendor = "apple")]
unsafe fn stream_socket() -> std::io::Result<i32> {
    let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
        let error = std::io::Error::last_os_error();
        libc::close(fd);
        return Err(error);
    }
    Ok(fd)
}

#[cfg(not(target_vendor = "apple"))]
fn sockaddr(v4: &SocketAddrV4) -> libc::sockaddr_in {
    libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: v4.port().to_be(),
        sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(v4.ip().octets()) },
        sin_zero: [0; 8],
    }
}

#[cfg(target_vendor = "apple")]
fn sockaddr(v4: &SocketAddrV4) -> libc::sockaddr_in {
    libc::sockaddr_in {
        sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: v4.port().to_be(),
        sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(v4.ip().octets()) },
        sin_zero: [0; 8],
    }
}

/// `socket` + `bind` + `listen(backlog)` without `SO_REUSEADDR`.
pub fn bind_tcp_v4(v4: SocketAddrV4, backlog: i32) -> std::io::Result<TcpListener> {
    // SAFETY: the fd is owned by the TcpListener from creation on (closed on
    // every error path by its Drop); sockaddr is a valid sockaddr_in.
    unsafe {
        let fd = stream_socket()?;
        let listener = TcpListener::from_raw_fd(fd);
        let sin = sockaddr(&v4);
        if libc::bind(fd, &sin as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::listen(fd, backlog) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(listener)
    }
}

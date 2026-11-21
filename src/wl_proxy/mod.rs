//! The supervisor's Wayland proxy: apps connect to the supervisor, which forwards each connection
//! to the compositor over a socket pair of its own.
//!
//! The point (see "Restarting in place" in ARBOR.md) is that the app's connection belongs to the
//! supervisor, which outlives compositors. This is stage 0: everything is forwarded as it comes
//! and decoded on the side ([`wire`]), the compositor learns each app's real pid/uid/gid, and a
//! compositor that goes away still ends its apps' connections. Later stages replay each app's
//! objects into the next compositor instead.
//!
//! Each app gets a thread. A new connection is handed to the compositor through the "clients"
//! channel, a `SOCK_SEQPACKET` socket pair made for each compositor process: one message per
//! connection, carrying the app's credentials (12 bytes: pid, uid, gid) and the compositor's end
//! of the connection as an fd. A connection made while no compositor is running waits for the
//! next one.

pub mod interfaces;
pub mod wire;

use std::collections::VecDeque;
use std::mem::{size_of, MaybeUninit};
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use std::{io, thread};

use self::wire::{Dir, Tracker};

/// The most fds one `recvmsg` can carry (`SCM_MAX_FD`).
const MAX_FDS: usize = 253;
/// An app or compositor that stops reading while this much is queued for it is disconnected.
const MAX_QUEUED: usize = 32 << 20;

/// Credentials of the process at the other end of a unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Creds {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

impl Creds {
    fn to_bytes(self) -> [u8; 12] {
        let mut out = [0; 12];
        out[0..4].copy_from_slice(&self.pid.to_ne_bytes());
        out[4..8].copy_from_slice(&self.uid.to_ne_bytes());
        out[8..12].copy_from_slice(&self.gid.to_ne_bytes());
        out
    }

    fn from_bytes(b: &[u8]) -> Option<Self> {
        let b: &[u8; 12] = b.try_into().ok()?;
        Some(Self {
            pid: i32::from_ne_bytes(b[0..4].try_into().unwrap()),
            uid: u32::from_ne_bytes(b[4..8].try_into().unwrap()),
            gid: u32::from_ne_bytes(b[8..12].try_into().unwrap()),
        })
    }
}

pub fn peer_creds(fd: BorrowedFd) -> io::Result<Creds> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid for getsockopt to write.
    let ret = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Creds {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// A `SOCK_SEQPACKET` socket pair (close-on-exec) for the clients channel.
pub fn seqpacket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: fds is valid for two ints.
    let ret = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair gave us these two fds.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

// Room for MAX_FDS fds in a control message, aligned for cmsghdr.
#[repr(C, align(8))]
struct CmsgBuf([u8; 1056]);
const _: () = assert!(size_of::<CmsgBuf>() >= 16 + MAX_FDS * 4);

/// `recvmsg` without blocking; received fds are appended to `fds`. `Ok(0)` is end of stream.
pub fn recv_with_fds(fd: BorrowedFd, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut cmsg = MaybeUninit::<CmsgBuf>::zeroed();
    // SAFETY: zeroed msghdr is valid.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.as_mut_ptr().cast();
    msg.msg_controllen = size_of::<CmsgBuf>() as _;
    let n = loop {
        // SAFETY: msg points to valid buffers for the duration of the call.
        let n = unsafe {
            libc::recvmsg(
                fd.as_raw_fd(),
                &mut msg,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if n >= 0 {
            break n as usize;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    };
    // SAFETY: walking the control messages recvmsg filled in.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(c).cast::<libc::c_int>();
                let len = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                for i in 0..len / size_of::<libc::c_int>() {
                    fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::other("control message truncated (too many fds)"));
    }
    Ok(n)
}

/// `sendmsg` without blocking, with `fds` attached to the first byte.
pub fn send_with_fds(fd: BorrowedFd, bytes: &[u8], fds: &[OwnedFd]) -> io::Result<usize> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut _,
        iov_len: bytes.len(),
    };
    let mut cmsg = MaybeUninit::<CmsgBuf>::zeroed();
    // SAFETY: zeroed msghdr is valid.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        assert!(fds.len() <= MAX_FDS);
        let data_len = std::mem::size_of_val(fds) as u32;
        // SAFETY: filling in one SCM_RIGHTS control message inside cmsg's buffer.
        unsafe {
            msg.msg_control = cmsg.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(data_len) as _;
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(data_len) as _;
            let data = libc::CMSG_DATA(c).cast::<libc::c_int>();
            for (i, fd) in fds.iter().enumerate() {
                data.add(i).write_unaligned(fd.as_raw_fd());
            }
        }
    }
    loop {
        // SAFETY: msg points to valid buffers for the duration of the call.
        let n = unsafe {
            libc::sendmsg(
                fd.as_raw_fd(),
                &msg,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Reads one new connection from the clients channel (compositor side). `Ok(None)`: nothing
/// waiting.
pub fn recv_client(chan: BorrowedFd) -> io::Result<Option<(UnixStream, Creds)>> {
    let mut buf = [0u8; 12];
    let mut fds = Vec::new();
    loop {
        match recv_with_fds(chan, &mut buf, &mut fds) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                let creds = Creds::from_bytes(&buf[..n]);
                let fd = fds.pop();
                fds.clear();
                match (creds, fd) {
                    (Some(creds), Some(fd)) => return Ok(Some((UnixStream::from(fd), creds))),
                    // A malformed message: skip it.
                    _ => continue,
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(err) => return Err(err),
        }
    }
}

/// Where new connections go: the current compositor's clients channel, if one is running.
#[derive(Default)]
pub struct Hub {
    chan: Mutex<Option<Arc<OwnedFd>>>,
    changed: Condvar,
}

impl Hub {
    /// Sets (a compositor started) or clears (it ended) the clients channel.
    pub fn set(&self, chan: Option<OwnedFd>) {
        *self.chan.lock().unwrap() = chan.map(Arc::new);
        self.changed.notify_all();
    }

    /// Hands a new connection for an app with `creds` to the compositor, waiting for one if
    /// none is running. Returns the proxy's end.
    fn connect(&self, creds: Creds) -> io::Result<UnixStream> {
        loop {
            let chan = {
                let mut guard = self.chan.lock().unwrap();
                while guard.is_none() {
                    guard = self.changed.wait(guard).unwrap();
                }
                guard.clone().unwrap()
            };
            let (ours, theirs) = UnixStream::pair()?;
            match send_with_fds(chan.as_fd(), &creds.to_bytes(), &[OwnedFd::from(theirs)]) {
                Ok(_) => return Ok(ours),
                Err(err) => {
                    // The compositor is gone (or its channel is full): wait for the next.
                    debug!("wl-proxy: error handing a client to the compositor: {err}");
                    let guard = self.chan.lock().unwrap();
                    if guard.as_ref().is_some_and(|c| Arc::ptr_eq(c, &chan)) {
                        let _ = self
                            .changed
                            .wait_timeout(guard, Duration::from_millis(200))
                            .unwrap();
                    }
                }
            }
        }
    }
}

/// Accepts apps on `listener` (the session's Wayland socket) and proxies each one, forever.
pub fn start(listener: UnixListener, hub: Arc<Hub>) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    thread::Builder::new()
        .name("wl-proxy-accept".to_owned())
        .spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let hub = hub.clone();
                    if let Err(err) = thread::Builder::new()
                        .name("wl-proxy".to_owned())
                        .spawn(move || serve_client(stream, &hub))
                    {
                        warn!("wl-proxy: error starting a thread: {err}");
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    let _ = poll(&mut [(listener.as_fd(), libc::POLLIN)], -1);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => (),
                Err(err) => {
                    warn!("wl-proxy: error accepting: {err}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })?;
    Ok(())
}

fn process_name(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim_end().to_owned())
        .unwrap_or_else(|_| "?".to_owned())
}

fn serve_client(client: UnixStream, hub: &Hub) {
    let creds = match peer_creds(client.as_fd()) {
        Ok(c) => c,
        Err(err) => {
            warn!("wl-proxy: error reading the client's credentials: {err}");
            return;
        }
    };
    let label = format!("{} ({})", process_name(creds.pid), creds.pid);
    let server = match hub.connect(creds) {
        Ok(s) => s,
        Err(err) => {
            warn!("wl-proxy: {label}: error connecting to the compositor: {err}");
            return;
        }
    };
    debug!("wl-proxy: {label} connected");
    // A bug here must only cost this one app its connection.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Connection::new(client, server, label.clone()).run()
    }));
    match result {
        Ok(end) => debug!("wl-proxy: {label}: {end}"),
        Err(_) => warn!("wl-proxy: {label}: the proxy panicked; connection dropped"),
    }
}

fn poll(
    fds: &mut [(BorrowedFd, libc::c_short)],
    timeout_ms: i32,
) -> io::Result<Vec<libc::c_short>> {
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|(fd, events)| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: *events,
            revents: 0,
        })
        .collect();
    // SAFETY: pfds is a valid array of pollfd.
    let ret = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, timeout_ms) };
    if ret < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(vec![0; pfds.len()]);
        }
        return Err(err);
    }
    Ok(pfds.iter().map(|p| p.revents).collect())
}

/// Data waiting to be written to one side.
#[derive(Default)]
struct Outgoing {
    chunks: VecDeque<Chunk>,
    bytes: usize,
}

struct Chunk {
    data: Vec<u8>,
    sent: usize,
    fds: Vec<OwnedFd>,
}

impl Outgoing {
    fn push(&mut self, data: &[u8], fds: Vec<OwnedFd>) {
        self.bytes += data.len();
        // Coalesce with the last chunk when no fds are involved.
        if fds.is_empty() {
            if let Some(last) = self.chunks.back_mut() {
                if last.sent == 0 || last.fds.is_empty() {
                    last.data.extend_from_slice(data);
                    return;
                }
            }
        }
        self.chunks.push_back(Chunk {
            data: data.to_vec(),
            sent: 0,
            fds,
        });
    }

    fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Writes as much as the socket takes.
    fn flush(&mut self, fd: BorrowedFd) -> io::Result<()> {
        while let Some(chunk) = self.chunks.front_mut() {
            match send_with_fds(fd, &chunk.data[chunk.sent..], &chunk.fds) {
                Ok(n) => {
                    chunk.fds.clear();
                    chunk.sent += n;
                    self.bytes -= n;
                    if chunk.sent == chunk.data.len() {
                        self.chunks.pop_front();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// Why a connection ended.
#[derive(Debug)]
enum End {
    ClientClosed,
    CompositorClosed(Option<String>),
    Error(&'static str, io::Error),
    TooMuchQueued(&'static str),
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            End::ClientClosed => write!(f, "disconnected"),
            End::CompositorClosed(None) => write!(f, "the compositor closed the connection"),
            End::CompositorClosed(Some(err)) => {
                write!(
                    f,
                    "the compositor closed the connection after a protocol error: {err}"
                )
            }
            End::Error(side, err) => write!(f, "error on the {side} side: {err}"),
            End::TooMuchQueued(side) => write!(f, "the {side} stopped reading"),
        }
    }
}

struct Connection {
    client: UnixStream,
    server: UnixStream,
    label: String,
    tracker: Tracker,
    warned_lost: bool,
    to_client: Outgoing,
    to_server: Outgoing,
}

enum Read {
    Open,
    Closed,
}

impl Connection {
    fn new(client: UnixStream, server: UnixStream, label: String) -> Self {
        Self {
            client,
            server,
            label,
            tracker: Tracker::default(),
            warned_lost: false,
            to_client: Outgoing::default(),
            to_server: Outgoing::default(),
        }
    }

    /// Reads everything available from one side and queues it for the other.
    fn read(&mut self, dir: Dir, buf: &mut [u8]) -> io::Result<Read> {
        let (from, to) = match dir {
            Dir::Request => (&self.client, &mut self.to_server),
            Dir::Event => (&self.server, &mut self.to_client),
        };
        loop {
            let mut fds = Vec::new();
            match recv_with_fds(from.as_fd(), buf, &mut fds) {
                Ok(0) => return Ok(Read::Closed),
                Ok(n) => {
                    self.tracker.feed(dir, &buf[..n], fds.len());
                    to.push(&buf[..n], fds);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(Read::Open),
                Err(err) if err.kind() == io::ErrorKind::ConnectionReset => {
                    return Ok(Read::Closed)
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn note_lost(&mut self) {
        if !self.warned_lost {
            if let Some(err) = &self.tracker.lost {
                self.warned_lost = true;
                warn!(
                    "wl-proxy: {}: lost track of the protocol ({err}); this app won't survive a restart",
                    self.label
                );
            }
        }
    }

    fn run(mut self) -> End {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut client_events = libc::POLLIN;
            if !self.to_client.is_empty() {
                client_events |= libc::POLLOUT;
            }
            let mut server_events = libc::POLLIN;
            if !self.to_server.is_empty() {
                server_events |= libc::POLLOUT;
            }
            let revents = match poll(
                &mut [
                    (self.client.as_fd(), client_events),
                    (self.server.as_fd(), server_events),
                ],
                -1,
            ) {
                Ok(r) => r,
                Err(err) => return End::Error("poll", err),
            };
            let readable =
                |r: libc::c_short| r & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;

            if readable(revents[0]) {
                match self.read(Dir::Request, &mut buf) {
                    Ok(Read::Open) => (),
                    Ok(Read::Closed) => return End::ClientClosed,
                    Err(err) => return End::Error("app", err),
                }
            }
            if readable(revents[1]) {
                match self.read(Dir::Event, &mut buf) {
                    Ok(Read::Open) => (),
                    Ok(Read::Closed) => {
                        self.note_lost();
                        self.drain_to_client();
                        return End::CompositorClosed(self.tracker.error.take());
                    }
                    Err(err) => return End::Error("compositor", err),
                }
            }
            self.note_lost();

            if let Err(err) = self.to_server.flush(self.server.as_fd()) {
                if err.kind() == io::ErrorKind::BrokenPipe {
                    self.drain_to_client();
                    return End::CompositorClosed(self.tracker.error.take());
                }
                return End::Error("compositor", err);
            }
            if let Err(err) = self.to_client.flush(self.client.as_fd()) {
                if err.kind() == io::ErrorKind::BrokenPipe {
                    return End::ClientClosed;
                }
                return End::Error("app", err);
            }
            if self.to_client.bytes > MAX_QUEUED {
                return End::TooMuchQueued("app");
            }
            if self.to_server.bytes > MAX_QUEUED {
                return End::TooMuchQueued("compositor");
            }
        }
    }

    /// After the compositor closed: passes on what it sent last (a protocol error, say).
    fn drain_to_client(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !self.to_client.is_empty() && Instant::now() < deadline {
            if self.to_client.flush(self.client.as_fd()).is_err() {
                return;
            }
            if !self.to_client.is_empty() {
                let _ = poll(&mut [(self.client.as_fd(), libc::POLLOUT)], 100);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fds_cross_a_socket() {
        let (a, b) = UnixStream::pair().unwrap();
        let (x, _y) = UnixStream::pair().unwrap();
        let n = send_with_fds(a.as_fd(), b"hello", &[OwnedFd::from(x)]).unwrap();
        assert_eq!(n, 5);
        let mut buf = [0u8; 16];
        let mut fds = Vec::new();
        let n = recv_with_fds(b.as_fd(), &mut buf, &mut fds).unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert_eq!(fds.len(), 1);
    }

    #[test]
    fn clients_channel() {
        let (sup, comp) = seqpacket_pair().unwrap();
        let hub = Hub::default();
        hub.set(Some(sup));
        let creds = Creds {
            pid: 42,
            uid: 1000,
            gid: 100,
        };
        let mut ours = hub.connect(creds).unwrap();
        let (mut theirs, got) = recv_client(comp.as_fd()).unwrap().unwrap();
        assert_eq!(got, creds);
        use std::io::{Read as _, Write as _};
        ours.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        theirs.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
        assert!(recv_client(comp.as_fd()).unwrap().is_none());
    }
}

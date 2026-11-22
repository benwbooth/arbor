//! The supervisor proxies arbor's IPC socket too: its connections (hearth's event streams,
//! `arbor msg`, scripts) outlive compositors.
//!
//! The protocol is line based: a request line gets one reply line; after an `EventStream`
//! request the compositor sends event lines. When the compositor goes away, the connection is
//! taken to the next one: an event stream asks for `EventStream` again (its reply line is kept
//! from the app; the new compositor's first events send the whole state again), and requests
//! still waiting for their reply are sent again. Requests that leave state in the compositor
//! (`SetGlass` shapes, temporary `Output` changes) are sent again too, the latest of each.

use std::collections::VecDeque;
use std::os::fd::AsFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::Duration;
use std::{io, thread};

use super::{poll, recv_with_fds, send_with_fds, Hub};

/// Accepts IPC clients on `listener` (the session's IPC socket) and proxies each, forever.
pub fn start(listener: UnixListener, hub: Arc<Hub>) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    thread::Builder::new()
        .name("ipc-proxy-accept".to_owned())
        .spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let hub = hub.clone();
                    let _ = thread::Builder::new()
                        .name("ipc-proxy".to_owned())
                        .spawn(move || {
                            let res =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    serve(stream, &hub)
                                }));
                            if res.is_err() {
                                warn!("ipc-proxy: panicked; connection dropped");
                            }
                        });
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    let _ = poll(&mut [(listener.as_fd(), libc::POLLIN)], -1);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => (),
                Err(err) => {
                    warn!("ipc-proxy: error accepting: {err}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })?;
    Ok(())
}

/// Splits a byte stream into lines (newline included).
#[derive(Default)]
struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            out.push(self.buf.drain(..=i).collect());
        }
        out
    }
}

/// For requests that leave state in the compositor: which state (the latest request per key is
/// sent to the next compositor).
fn state_key(line: &[u8]) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_slice(line).ok()?;
    if let Some(glass) = v.get_mut("SetGlass") {
        glass.as_object_mut()?.remove("shapes");
        return Some(format!("SetGlass:{glass}"));
    }
    let out = v.get("Output")?;
    let action = out.get("action")?;
    let kind = match action {
        serde_json::Value::Object(m) => m.keys().next()?.clone(),
        other => other.to_string(),
    };
    Some(format!("Output:{}:{kind}", out.get("output")?))
}

fn is_event_stream_request(line: &[u8]) -> bool {
    line.trim_ascii() == b"\"EventStream\""
}

fn write_all(stream: &UnixStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match send_with_fds(stream.as_fd(), bytes, &[]) {
            Ok(n) => bytes = &bytes[n..],
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                poll(&mut [(stream.as_fd(), libc::POLLOUT)], 1000)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn serve(client: UnixStream, hub: &Hub) {
    let Ok((mut server, mut generation)) = hub.connect_ipc(0) else {
        return;
    };
    let _ = client.set_nonblocking(true);
    let _ = server.set_nonblocking(true);
    let mut from_client = Lines::default();
    let mut from_server = Lines::default();
    // Requests sent and not answered yet.
    let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
    // The event stream request, once answered: the connection is a stream now.
    let mut stream: Option<Vec<u8>> = None;
    // Reply lines to keep from the app (to requests the proxy sent again for a stream).
    let mut skip = 0usize;
    // The latest request per piece of compositor state (state_key).
    let mut state: Vec<(String, Vec<u8>)> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    // The client shut down its writing side after its request (`arbor msg` does): passed on to
    // the compositor (and to the next one).
    let mut client_eof = false;

    loop {
        let client_events = if client_eof { 0 } else { libc::POLLIN };
        let Ok(revents) = poll(
            &mut [
                (client.as_fd(), client_events),
                (server.as_fd(), libc::POLLIN),
            ],
            -1,
        ) else {
            return;
        };
        let readable = |r: libc::c_short| r & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;

        if !client_eof && readable(revents[0]) {
            let mut fds = Vec::new();
            match recv_with_fds(client.as_fd(), &mut buf, &mut fds) {
                Ok(0) => {
                    client_eof = true;
                    let _ = server.shutdown(std::net::Shutdown::Write);
                }
                Ok(n) => {
                    for line in from_client.push(&buf[..n]) {
                        if let Some(key) = state_key(&line) {
                            state.retain(|(k, _)| *k != key);
                            state.push((key, line.clone()));
                        }
                        if write_all(&server, &line).is_err() {
                            // Sent again on the next compositor.
                        }
                        pending.push_back(line);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => (),
                Err(err) => {
                    debug!("ipc-proxy: client error: {err}");
                    return;
                }
            }
        }

        let mut gone = false;
        if readable(revents[1]) {
            let mut fds = Vec::new();
            match recv_with_fds(server.as_fd(), &mut buf, &mut fds) {
                Ok(0) => gone = true,
                Ok(n) => {
                    for line in from_server.push(&buf[..n]) {
                        if skip > 0 {
                            skip -= 1;
                            continue;
                        }
                        if stream.is_none() {
                            if let Some(request) = pending.pop_front() {
                                if is_event_stream_request(&request) {
                                    stream = Some(request);
                                }
                            }
                        }
                        if let Err(err) = write_all(&client, &line) {
                            debug!("ipc-proxy: error writing to the client: {err}");
                            return;
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => (),
                Err(_) => gone = true,
            }
        }

        if gone {
            // Everything answered and nothing more to come: the end of a one-shot request.
            if client_eof && pending.is_empty() && stream.is_none() {
                return;
            }
            // The compositor went away (or closed this connection on purpose).
            if !hub.wait_gone(generation, Duration::from_secs(3)) {
                debug!("ipc-proxy: the compositor closed the connection");
                return;
            }
            let Ok((next, g)) = hub.connect_ipc(generation) else {
                return;
            };
            server = next;
            generation = g;
            let _ = server.set_nonblocking(true);
            from_server = Lines::default();
            // Replies come in request order: the stream's and the state's first (kept from the
            // app), then the pending requests' (passed on).
            skip = 0;
            if let Some(request) = &stream {
                skip += 1;
                let _ = write_all(&server, request);
            }
            for (_, request) in &state {
                // Not if it's still pending: then it goes below, as the app's.
                if pending.contains(request) {
                    continue;
                }
                skip += 1;
                let _ = write_all(&server, request);
            }
            for request in &pending {
                let _ = write_all(&server, request);
            }
            if client_eof {
                let _ = server.shutdown(std::net::Shutdown::Write);
            }
            debug!("ipc-proxy: connection carried over to the new compositor");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split() {
        let mut l = Lines::default();
        assert!(l.push(b"\"Event").is_empty());
        assert_eq!(
            l.push(b"Stream\"\n{\"a\":1}\n{"),
            vec![b"\"EventStream\"\n".to_vec(), b"{\"a\":1}\n".to_vec()]
        );
        assert!(is_event_stream_request(b"\"EventStream\"\n"));
        assert!(!is_event_stream_request(b"\"Windows\"\n"));
        let a = state_key(br#"{"SetGlass":{"namespace":"bar","output":null,"shapes":[1]}}"#);
        let b = state_key(br#"{"SetGlass":{"namespace":"bar","output":null,"shapes":[2,3]}}"#);
        assert!(a.is_some() && a == b);
        assert_ne!(
            a,
            state_key(br#"{"SetGlass":{"namespace":"dock","output":null,"shapes":[]}}"#)
        );
        assert_eq!(
            state_key(br#"{"Output":{"output":"DP-1","action":{"Scale":{"scale":1.5}}}}"#)
                .as_deref(),
            Some(r#"Output:"DP-1":Scale"#)
        );
        assert!(state_key(br#""Windows""#).is_none());
    }
}

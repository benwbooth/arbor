//! The Wayland wire format: splitting each direction's byte stream into messages and keeping a
//! table of the connection's objects (interface and version per id).
//!
//! Stage 0 of the proxy only observes: bytes and fds are forwarded as they arrive, and the
//! decoder follows along. If it ever loses track (an object or interface it doesn't know), the
//! connection keeps working, it just can't be carried over to a new compositor.

use std::collections::HashMap;
use std::fmt;

use wayland_backend::protocol::{ArgumentType, Interface, MessageDesc};

use super::interfaces;

/// Who sent a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Client to compositor: a request.
    Request,
    /// Compositor to client: an event.
    Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub object: u32,
    pub opcode: u16,
    /// Of the whole message, header included.
    pub size: usize,
}

pub const HEADER_SIZE: usize = 8;

/// libwayland's limit (the size field is 16 bits).
pub const MAX_MESSAGE_SIZE: usize = 0xffff;

pub fn parse_header(buf: &[u8]) -> Option<Header> {
    if buf.len() < HEADER_SIZE {
        return None;
    }
    let object = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
    let word = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
    Some(Header {
        object,
        opcode: (word & 0xffff) as u16,
        size: (word >> 16) as usize,
    })
}

/// A decoded argument (strings and arrays borrow the message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg<'a> {
    Int(i32),
    Uint(u32),
    Fixed(i32),
    /// Without the trailing NUL; `None` for a null string.
    Str(Option<&'a [u8]>),
    Object(u32),
    NewId(u32),
    Array(&'a [u8]),
    Fd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    BadSize(Header),
    Truncated {
        interface: &'static str,
        message: &'static str,
    },
    UnknownObject {
        id: u32,
        dir: Dir,
    },
    BadOpcode {
        interface: &'static str,
        opcode: u16,
        dir: Dir,
    },
    UnknownInterface(String),
    MissingFd {
        interface: &'static str,
        message: &'static str,
    },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::BadSize(h) => write!(f, "bad message size {} (object {})", h.size, h.object),
            WireError::Truncated { interface, message } => {
                write!(f, "truncated {interface}.{message}")
            }
            WireError::UnknownObject { id, dir } => write!(f, "{dir:?} to unknown object {id}"),
            WireError::BadOpcode {
                interface,
                opcode,
                dir,
            } => write!(f, "{dir:?} with bad opcode {opcode} on {interface}"),
            WireError::UnknownInterface(name) => write!(f, "unknown interface {name}"),
            WireError::MissingFd { interface, message } => {
                write!(f, "{interface}.{message} without its fd")
            }
        }
    }
}

fn read_u32(body: &[u8], pos: &mut usize) -> Option<u32> {
    let bytes = body.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_ne_bytes(bytes.try_into().unwrap()))
}

fn read_blob<'a>(body: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len = read_u32(body, pos)? as usize;
    let padded = len.checked_add(3)? & !3;
    let bytes = body.get(*pos..*pos + len)?;
    if *pos + padded > body.len() {
        return None;
    }
    *pos += padded;
    Some(bytes)
}

/// Decodes a message body (after the header) by its signature.
pub fn parse_args<'a>(signature: &[ArgumentType], body: &'a [u8]) -> Option<Vec<Arg<'a>>> {
    let mut pos = 0;
    let mut args = Vec::with_capacity(signature.len());
    for ty in signature {
        let arg = match ty {
            ArgumentType::Int => Arg::Int(read_u32(body, &mut pos)? as i32),
            ArgumentType::Uint => Arg::Uint(read_u32(body, &mut pos)?),
            ArgumentType::Fixed => Arg::Fixed(read_u32(body, &mut pos)? as i32),
            ArgumentType::Object(_) => Arg::Object(read_u32(body, &mut pos)?),
            ArgumentType::NewId => Arg::NewId(read_u32(body, &mut pos)?),
            ArgumentType::Str(_) => {
                let blob = read_blob(body, &mut pos)?;
                match blob {
                    [] => Arg::Str(None),
                    [s @ .., 0] => Arg::Str(Some(s)),
                    // Not NUL-terminated.
                    _ => return None,
                }
            }
            ArgumentType::Array => Arg::Array(read_blob(body, &mut pos)?),
            ArgumentType::Fd => Arg::Fd,
        };
        args.push(arg);
    }
    (pos == body.len()).then_some(args)
}

#[derive(Debug, Clone, Copy)]
pub struct Object {
    pub interface: &'static Interface,
    pub version: u32,
    /// False once destroyed; kept so that messages already on the way can still be decoded.
    pub alive: bool,
}

/// One decoded message, as the tracker saw it.
#[derive(Debug)]
pub struct Decoded<'a> {
    pub header: Header,
    pub interface: &'static Interface,
    pub desc: &'static MessageDesc,
    pub args: Vec<Arg<'a>>,
}

impl Decoded<'_> {
    pub fn fd_count(&self) -> usize {
        self.args.iter().filter(|a| matches!(a, Arg::Fd)).count()
    }
}

/// The objects of one connection.
#[derive(Debug)]
pub struct Objects {
    map: HashMap<u32, Object>,
}

impl Default for Objects {
    fn default() -> Self {
        let mut map = HashMap::new();
        map.insert(
            1,
            Object {
                interface: interfaces::wl_display(),
                version: 1,
                alive: true,
            },
        );
        Self { map }
    }
}

impl Objects {
    pub fn get(&self, id: u32) -> Option<&Object> {
        self.map.get(&id)
    }

    pub fn live_count(&self) -> usize {
        self.map.values().filter(|o| o.alive).count()
    }

    /// Decodes one complete message (`msg` includes the header) and updates the object table.
    pub fn handle<'a>(&mut self, dir: Dir, msg: &'a [u8]) -> Result<Decoded<'a>, WireError> {
        let header = parse_header(msg).expect("caller passes complete messages");
        let object = *self
            .map
            .get(&header.object)
            .ok_or(WireError::UnknownObject {
                id: header.object,
                dir,
            })?;
        let interface = object.interface;
        let list = match dir {
            Dir::Request => interface.requests,
            Dir::Event => interface.events,
        };
        let desc = list
            .get(header.opcode as usize)
            .ok_or(WireError::BadOpcode {
                interface: interface.name,
                opcode: header.opcode,
                dir,
            })?;
        let args = parse_args(desc.signature, &msg[HEADER_SIZE..]).ok_or(WireError::Truncated {
            interface: interface.name,
            message: desc.name,
        })?;

        // New objects. A new_id without a fixed interface (wl_registry.bind) is preceded by the
        // interface name and version.
        for (i, arg) in args.iter().enumerate() {
            let Arg::NewId(id) = *arg else { continue };
            let (child, version) = match desc.child_interface {
                Some(child) => (child, object.version),
                None => {
                    let (Some(Arg::Str(Some(name))), Some(Arg::Uint(version))) = (
                        i.checked_sub(2).map(|j| &args[j]),
                        i.checked_sub(1).map(|j| &args[j]),
                    ) else {
                        return Err(WireError::Truncated {
                            interface: interface.name,
                            message: desc.name,
                        });
                    };
                    let name = String::from_utf8_lossy(name);
                    let child = interfaces::by_name(&name)
                        .ok_or_else(|| WireError::UnknownInterface(name.into_owned()))?;
                    (child, *version)
                }
            };
            if id == 0 {
                continue;
            }
            self.map.insert(
                id,
                Object {
                    interface: child,
                    version,
                    alive: true,
                },
            );
        }

        if desc.is_destructor {
            if let Some(obj) = self.map.get_mut(&header.object) {
                obj.alive = false;
            }
        }

        Ok(Decoded {
            header,
            interface,
            desc,
            args,
        })
    }
}

/// Splits one direction's byte stream into messages.
#[derive(Debug, Default)]
pub struct Splitter {
    buf: Vec<u8>,
    /// Fds received and not yet claimed by a message.
    pub fds: usize,
}

impl Splitter {
    /// Adds received bytes and fds, then calls `f` with each complete message.
    pub fn feed(
        &mut self,
        bytes: &[u8],
        fds: usize,
        mut f: impl FnMut(&[u8], &mut usize) -> Result<(), WireError>,
    ) -> Result<(), WireError> {
        self.fds += fds;
        self.buf.extend_from_slice(bytes);
        let mut start = 0;
        let result = loop {
            let rest = &self.buf[start..];
            let Some(header) = parse_header(rest) else {
                break Ok(());
            };
            if header.size < HEADER_SIZE || header.size % 4 != 0 {
                break Err(WireError::BadSize(header));
            }
            if rest.len() < header.size {
                break Ok(());
            }
            if let Err(err) = f(&rest[..header.size], &mut self.fds) {
                break Err(err);
            }
            start += header.size;
        };
        self.buf.drain(..start);
        result
    }
}

/// Decoding both directions of one connection.
#[derive(Debug, Default)]
pub struct Tracker {
    pub objects: Objects,
    requests: Splitter,
    events: Splitter,
    /// Set once the tracker lost track; then it stops decoding.
    pub lost: Option<WireError>,
    /// The last protocol error the compositor sent, for the log.
    pub error: Option<String>,
}

impl Tracker {
    /// Follows bytes (and the number of fds) that went through in direction `dir`.
    pub fn feed(&mut self, dir: Dir, bytes: &[u8], fds: usize) {
        if self.lost.is_some() {
            return;
        }
        let Tracker {
            objects,
            requests,
            events,
            error,
            ..
        } = self;
        let splitter = match dir {
            Dir::Request => requests,
            Dir::Event => events,
        };
        let result = splitter.feed(bytes, fds, |msg, fds| {
            let decoded = objects.handle(dir, msg)?;
            let needed = decoded.fd_count();
            if needed > *fds {
                return Err(WireError::MissingFd {
                    interface: decoded.interface.name,
                    message: decoded.desc.name,
                });
            }
            *fds -= needed;
            if dir == Dir::Event && decoded.header.object == 1 && decoded.desc.name == "error" {
                if let [Arg::Object(id), Arg::Uint(code), Arg::Str(msg)] = decoded.args[..] {
                    let iface = objects.get(id).map_or("?", |o| o.interface.name);
                    *error = Some(format!(
                        "{iface}#{id} code {code}: {}",
                        String::from_utf8_lossy(msg.unwrap_or_default())
                    ));
                }
            }
            Ok(())
        });
        if let Err(err) = result {
            self.lost = Some(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(object: u32, opcode: u16, args: &[u32]) -> Vec<u8> {
        let size = HEADER_SIZE + args.len() * 4;
        let mut out = Vec::new();
        out.extend_from_slice(&object.to_ne_bytes());
        out.extend_from_slice(&(((size as u32) << 16) | opcode as u32).to_ne_bytes());
        for a in args {
            out.extend_from_slice(&a.to_ne_bytes());
        }
        out
    }

    fn string(s: &str) -> Vec<u32> {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        let len = bytes.len() as u32;
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let mut out = vec![len];
        out.extend(
            bytes
                .chunks(4)
                .map(|c| u32::from_ne_bytes(c.try_into().unwrap())),
        );
        out
    }

    #[test]
    fn registry_bind_and_surface() {
        let mut t = Tracker::default();
        // wl_display.get_registry(new_id 2)
        let mut bytes = msg(1, 1, &[2]);
        // wl_registry.bind(name 1, "wl_compositor", 6, new_id 3)
        let mut args = vec![1];
        args.extend(string("wl_compositor"));
        args.extend([6, 3]);
        bytes.extend(msg(2, 0, &args));
        // wl_compositor.create_surface(new_id 4)
        bytes.extend(msg(3, 0, &[4]));
        // Fed in awkward pieces.
        for chunk in bytes.chunks(5) {
            t.feed(Dir::Request, chunk, 0);
        }
        assert_eq!(t.lost, None);
        let surface = t.objects.get(4).unwrap();
        assert_eq!(surface.interface.name, "wl_surface");
        assert_eq!(surface.version, 6);
        // wl_surface.destroy
        t.feed(Dir::Request, &msg(4, 0, &[]), 0);
        assert!(!t.objects.get(4).unwrap().alive);
    }

    #[test]
    fn fds_are_counted() {
        let mut t = Tracker::default();
        let mut args = vec![1];
        args.extend(string("wl_shm"));
        args.extend([1, 3]);
        let mut bytes = msg(1, 1, &[2]);
        bytes.extend(msg(2, 0, &args));
        t.feed(Dir::Request, &bytes, 0);
        // wl_shm.create_pool(new_id 4, fd, size 4096) without the fd.
        t.feed(Dir::Request, &msg(3, 0, &[4, 4096]), 0);
        assert!(matches!(t.lost, Some(WireError::MissingFd { .. })));

        let mut t = Tracker::default();
        t.feed(Dir::Request, &bytes, 0);
        t.feed(Dir::Request, &msg(3, 0, &[4, 4096]), 1);
        assert_eq!(t.lost, None);
        assert_eq!(t.objects.get(4).unwrap().interface.name, "wl_shm_pool");
    }

    #[test]
    fn unknown_object() {
        let mut t = Tracker::default();
        t.feed(Dir::Request, &msg(7, 0, &[]), 0);
        assert!(matches!(
            t.lost,
            Some(WireError::UnknownObject { id: 7, .. })
        ));
    }

    #[test]
    fn protocol_error_is_recorded() {
        let mut t = Tracker::default();
        let mut args = vec![1, 3];
        args.extend(string("bad"));
        t.feed(Dir::Event, &msg(1, 0, &args), 0);
        assert_eq!(t.lost, None);
        assert_eq!(t.error.as_deref(), Some("wl_display#1 code 3: bad"));
    }
}

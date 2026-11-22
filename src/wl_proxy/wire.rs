//! The Wayland wire format: splitting a byte stream into messages, decoding them by their
//! signatures and encoding them again.

use std::collections::VecDeque;
use std::fmt;
use std::os::fd::OwnedFd;

use wayland_backend::protocol::ArgumentType;

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

/// Ids from here up are allocated by the compositor.
pub const SERVER_ID_START: u32 = 0xff00_0000;

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

/// A decoded argument.
#[derive(Debug)]
pub enum A {
    Int(i32),
    Uint(u32),
    Fixed(i32),
    /// Without the trailing NUL; `None` for a null string.
    Str(Option<Vec<u8>>),
    /// 0 is null.
    Obj(u32),
    New(u32),
    Array(Vec<u8>),
    Fd(OwnedFd),
}

impl A {
    pub fn try_clone(&self) -> std::io::Result<A> {
        Ok(match self {
            A::Int(v) => A::Int(*v),
            A::Uint(v) => A::Uint(*v),
            A::Fixed(v) => A::Fixed(*v),
            A::Str(v) => A::Str(v.clone()),
            A::Obj(v) => A::Obj(*v),
            A::New(v) => A::New(*v),
            A::Array(v) => A::Array(v.clone()),
            A::Fd(fd) => A::Fd(fd.try_clone()?),
        })
    }
}

/// A decoded message.
#[derive(Debug)]
pub struct Msg {
    pub object: u32,
    pub opcode: u16,
    pub args: Vec<A>,
}

impl Msg {
    pub fn new(object: u32, opcode: u16, args: Vec<A>) -> Self {
        Self {
            object,
            opcode,
            args,
        }
    }

    /// The bytes and the fds to send with them.
    pub fn encode(self) -> (Vec<u8>, Vec<OwnedFd>) {
        let mut out = Vec::with_capacity(HEADER_SIZE + self.args.len() * 4);
        out.extend_from_slice(&self.object.to_ne_bytes());
        out.extend_from_slice(&[0; 4]);
        let mut fds = Vec::new();
        let blob = |out: &mut Vec<u8>, bytes: &[u8], nul: bool| {
            let len = bytes.len() + nul as usize;
            out.extend_from_slice(&(len as u32).to_ne_bytes());
            out.extend_from_slice(bytes);
            if nul {
                out.push(0);
            }
            while out.len() % 4 != 0 {
                out.push(0);
            }
        };
        for arg in self.args {
            match arg {
                A::Int(v) | A::Fixed(v) => out.extend_from_slice(&v.to_ne_bytes()),
                A::Uint(v) | A::Obj(v) | A::New(v) => out.extend_from_slice(&v.to_ne_bytes()),
                A::Str(None) => out.extend_from_slice(&0u32.to_ne_bytes()),
                A::Str(Some(s)) => blob(&mut out, &s, true),
                A::Array(a) => blob(&mut out, &a, false),
                A::Fd(fd) => fds.push(fd),
            }
        }
        let word = ((out.len() as u32) << 16) | self.opcode as u32;
        out[4..8].copy_from_slice(&word.to_ne_bytes());
        (out, fds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    BadSize(Header),
    TooLong,
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
            WireError::TooLong => write!(f, "message too long"),
            WireError::Truncated { interface, message } => {
                write!(f, "malformed {interface}.{message}")
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

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Malformed,
    MissingFd,
}

/// Decodes a message body (after the header) by its signature, taking its fds from `fds`.
pub fn decode_args(
    signature: &[ArgumentType],
    body: &[u8],
    fds: &mut VecDeque<OwnedFd>,
) -> Result<Vec<A>, DecodeError> {
    let needed = signature
        .iter()
        .filter(|t| matches!(t, ArgumentType::Fd))
        .count();
    if needed > fds.len() {
        return Err(DecodeError::MissingFd);
    }
    let mut pos = 0;
    let mut args = Vec::with_capacity(signature.len());
    for ty in signature {
        let m = DecodeError::Malformed;
        let arg = match ty {
            ArgumentType::Int => A::Int(read_u32(body, &mut pos).ok_or(m)? as i32),
            ArgumentType::Uint => A::Uint(read_u32(body, &mut pos).ok_or(m)?),
            ArgumentType::Fixed => A::Fixed(read_u32(body, &mut pos).ok_or(m)? as i32),
            ArgumentType::Object(_) => A::Obj(read_u32(body, &mut pos).ok_or(m)?),
            ArgumentType::NewId => A::New(read_u32(body, &mut pos).ok_or(m)?),
            ArgumentType::Str(_) => match read_blob(body, &mut pos).ok_or(m)? {
                [] => A::Str(None),
                [s @ .., 0] => A::Str(Some(s.to_vec())),
                _ => return Err(DecodeError::Malformed),
            },
            ArgumentType::Array => A::Array(read_blob(body, &mut pos).ok_or(m)?.to_vec()),
            // Checked above.
            ArgumentType::Fd => A::Fd(fds.pop_front().unwrap()),
        };
        args.push(arg);
    }
    if pos != body.len() {
        return Err(DecodeError::Malformed);
    }
    Ok(args)
}

/// Splits one direction's byte stream into messages.
#[derive(Debug, Default)]
pub struct Splitter {
    buf: Vec<u8>,
    start: usize,
    /// Fds received and not yet claimed by a message.
    pub fds: VecDeque<OwnedFd>,
}

impl Splitter {
    pub fn push(&mut self, bytes: &[u8], fds: Vec<OwnedFd>) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
        self.fds.extend(fds);
    }

    /// The next complete message: its header and body.
    pub fn next_msg(&mut self) -> Result<Option<(Header, Vec<u8>)>, WireError> {
        let rest = &self.buf[self.start..];
        let Some(header) = parse_header(rest) else {
            self.compact();
            return Ok(None);
        };
        if header.size < HEADER_SIZE || header.size % 4 != 0 {
            return Err(WireError::BadSize(header));
        }
        if rest.len() < header.size {
            self.compact();
            return Ok(None);
        }
        let body = rest[HEADER_SIZE..header.size].to_vec();
        self.start += header.size;
        Ok(Some((header, body)))
    }

    fn compact(&mut self) {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }

    /// Everything not yet split off (to pass on raw).
    pub fn take_rest(&mut self) -> (Vec<u8>, Vec<OwnedFd>) {
        let bytes = self.buf.split_off(self.start);
        self.buf.clear();
        self.start = 0;
        (bytes, self.fds.drain(..).collect())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn string(s: &str) -> Vec<u32> {
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
    fn encode_decode_roundtrip() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let msg = Msg::new(
            5,
            3,
            vec![
                A::Int(-2),
                A::Str(Some(b"hello".to_vec())),
                A::Str(None),
                A::Array(vec![1, 2, 3]),
                A::Fd(OwnedFd::from(a)),
                A::New(9),
            ],
        );
        let (bytes, fds) = msg.encode();
        assert_eq!(fds.len(), 1);
        let mut s = Splitter::default();
        s.push(&bytes[..7], vec![]);
        assert!(s.next_msg().unwrap().is_none());
        s.push(&bytes[7..], fds);
        let (h, body) = s.next_msg().unwrap().unwrap();
        assert_eq!((h.object, h.opcode, h.size), (5, 3, bytes.len()));
        use wayland_backend::protocol::AllowNull;
        let sig = [
            ArgumentType::Int,
            ArgumentType::Str(AllowNull::No),
            ArgumentType::Str(AllowNull::Yes),
            ArgumentType::Array,
            ArgumentType::Fd,
            ArgumentType::NewId,
        ];
        let args = decode_args(&sig, &body, &mut s.fds).unwrap();
        assert!(matches!(&args[1], A::Str(Some(s)) if s == b"hello"));
        assert!(matches!(&args[2], A::Str(None)));
        assert!(matches!(&args[3], A::Array(a) if a == &[1, 2, 3]));
        assert!(matches!(args[5], A::New(9)));
        assert!(s.next_msg().unwrap().is_none());
    }

    #[test]
    fn missing_fd() {
        let mut fds = VecDeque::new();
        assert_eq!(
            decode_args(&[ArgumentType::Fd], &[], &mut fds).unwrap_err(),
            DecodeError::MissingFd
        );
    }
}

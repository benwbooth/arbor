//! Id translation between the app's view and the current compositor's.
//!
//! Until the first compositor change the ids are the same on both sides and nothing is
//! translated. After it, the app keeps its ids while the proxy rebuilt its objects in the new
//! compositor under ids of the proxy's choosing; every message is translated from then on.
//! Global names are translated the same way.

use std::collections::HashMap;

use wayland_backend::protocol::{Interface, MessageDesc};

use super::wire::{Msg, A, SERVER_ID_START};

/// Client-view ids for compositor-made objects that would clash with ones the app still has.
const FRESH_SERVER_IDS: u32 = 0xff80_0000;

#[derive(Debug, Default)]
pub struct Ids {
    /// False until the first compositor change: everything is the same on both sides.
    pub active: bool,
    c2s: HashMap<u32, u32>,
    s2c: HashMap<u32, u32>,
    next_s: u32,
    free_s: Vec<u32>,
    /// Compositor ids of objects only the proxy uses (while rebuilding).
    pub temps: HashMap<u32, &'static Interface>,
    gc2s: HashMap<u32, u32>,
    gs2c: HashMap<u32, u32>,
    next_fresh: u32,
}

pub enum Fate {
    Keep,
    Swallow,
}

impl Ids {
    /// For a new compositor.
    pub fn reset(&mut self) {
        *self = Ids {
            active: true,
            next_s: 2,
            next_fresh: self.next_fresh.max(FRESH_SERVER_IDS),
            ..Default::default()
        };
        self.pair(1, 1);
    }

    /// A client-range id for a new object on the compositor side.
    pub fn alloc(&mut self) -> u32 {
        if let Some(id) = self.free_s.pop() {
            return id;
        }
        let id = self.next_s;
        self.next_s += 1;
        id
    }

    pub fn pair(&mut self, c: u32, s: u32) {
        self.c2s.insert(c, s);
        self.s2c.insert(s, c);
    }

    pub fn c2s(&self, c: u32) -> Option<u32> {
        if !self.active {
            return Some(c);
        }
        self.c2s.get(&c).copied()
    }

    pub fn s2c(&self, s: u32) -> Option<u32> {
        if !self.active {
            return Some(s);
        }
        self.s2c.get(&s).copied()
    }

    pub fn pair_global(&mut self, c: u32, s: u32) {
        self.gc2s.insert(c, s);
        self.gs2c.insert(s, c);
    }

    pub fn global_s2c(&self, s: u32) -> Option<u32> {
        self.gs2c.get(&s).copied()
    }

    pub fn global_c2s(&self, c: u32) -> Option<u32> {
        if !self.active {
            return Some(c);
        }
        self.gc2s.get(&c).copied()
    }

    /// Translates a request to the compositor's ids. False if something in it has no
    /// counterpart there (it must not be sent).
    pub fn request(&mut self, msg: &mut Msg, iface: &Interface, desc: &MessageDesc) -> bool {
        if !self.active {
            return true;
        }
        let Some(s) = self.c2s(msg.object) else {
            return false;
        };
        let c = msg.object;
        msg.object = s;
        if iface.name == "wl_registry" && desc.name == "bind" {
            let Some(A::Uint(name)) = msg.args.first_mut() else {
                return false;
            };
            let Some(sname) = self.gc2s.get(name) else {
                return false;
            };
            *name = *sname;
        }
        for arg in &mut msg.args {
            match arg {
                A::Obj(0) => (),
                A::Obj(id) => match self.c2s.get(id) {
                    Some(s) => *id = *s,
                    None => return false,
                },
                A::New(id) => {
                    let s = self.alloc();
                    self.pair(*id, s);
                    *id = s;
                }
                _ => (),
            }
        }
        // The compositor frees its own ids without a delete_id.
        if desc.is_destructor && c >= SERVER_ID_START {
            self.c2s.remove(&c);
            self.s2c.remove(&s);
        }
        true
    }

    /// Translates the arguments of an event (its object is already the client's).
    /// `in_use(id)`: whether the client has a live object with that id.
    pub fn event(
        &mut self,
        msg: &mut Msg,
        iface: &Interface,
        desc: &MessageDesc,
        in_use: impl Fn(u32) -> bool,
    ) -> Fate {
        if !self.active {
            return Fate::Keep;
        }
        match (iface.name, desc.name) {
            ("wl_display", "delete_id") => {
                let Some(A::Uint(id)) = msg.args.first_mut() else {
                    return Fate::Swallow;
                };
                let s = *id;
                if self.temps.remove(&s).is_some() {
                    self.free_s.push(s);
                    return Fate::Swallow;
                }
                let Some(c) = self.s2c.remove(&s) else {
                    return Fate::Swallow;
                };
                self.c2s.remove(&c);
                self.free_s.push(s);
                *id = c;
                return Fate::Keep;
            }
            ("wl_registry", "global") => {
                if let Some(A::Uint(name)) = msg.args.first_mut() {
                    let c = match self.gs2c.get(name) {
                        Some(c) => *c,
                        None => {
                            let used: std::collections::HashSet<u32> =
                                self.gc2s.keys().copied().collect();
                            let mut c = *name;
                            while used.contains(&c) {
                                c += 1;
                            }
                            self.pair_global(c, *name);
                            c
                        }
                    };
                    *name = c;
                }
                return Fate::Keep;
            }
            ("wl_registry", "global_remove") => {
                if let Some(A::Uint(name)) = msg.args.first_mut() {
                    match self.gs2c.remove(name) {
                        Some(c) => {
                            self.gc2s.remove(&c);
                            *name = c;
                        }
                        None => return Fate::Swallow,
                    }
                }
                return Fate::Keep;
            }
            _ => (),
        }
        for arg in &mut msg.args {
            match arg {
                A::Obj(0) => (),
                A::Obj(id) => *id = self.s2c.get(id).copied().unwrap_or(0),
                A::New(id) => {
                    let s = *id;
                    let c = if !in_use(s) && !self.c2s.contains_key(&s) {
                        s
                    } else {
                        let c = self.next_fresh;
                        self.next_fresh += 1;
                        c
                    };
                    self.pair(c, s);
                    *id = c;
                }
                _ => (),
            }
        }
        Fate::Keep
    }
}

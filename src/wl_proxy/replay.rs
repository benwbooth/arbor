//! Rebuilding an app's objects in a new compositor.
//!
//! 1. The proxy asks the new compositor for its globals (with a registry of its own) and matches
//!    them to the ones the app knows; globals that are gone are removed for the app.
//! 2. It recreates, in creation order, every live object it knows how to rebuild, plus destroyed
//!    ones they still depend on (the pool a buffer came from, say), then replays their state
//!    (title, app id, regions, viewport, ...).
//! 3. Toplevels get the usual first commit without a buffer; the proxy acks the configure that
//!    answers it (the app gets it too, and redraws at the new size), then attaches each surface's
//!    current buffer again and commits, children before parents.
//! 4. Objects only the proxy needed are destroyed again.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::os::fd::AsFd as _;
use std::time::{Duration, Instant};

use wayland_backend::protocol::{AllowNull, ArgumentType, Interface};

use super::model::{request_by_name, Creation, Key, Rec, V};
use super::wire::{Msg, A};
use super::{interfaces, poll, recv_with_fds, Connection, ReplayState};

const TIMEOUT: Duration = Duration::from_secs(10);

impl Connection {
    /// Queues a rebuilding request (logged at trace level).
    fn send_replay(&mut self, msg: Msg, iface: &'static Interface) {
        if tracing::enabled!(tracing::Level::TRACE) {
            let name = iface
                .requests
                .get(msg.opcode as usize)
                .map_or("?", |d| d.name);
            trace!(
                "wl-proxy: {}: replay {}#{}.{name} {:?}",
                self.label,
                iface.name,
                msg.object,
                msg.args
            );
        }
        self.to_server.push_msg(msg);
    }

    /// Sends what's queued for the compositor, waiting as needed.
    fn flush_server(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + TIMEOUT;
        while !self.to_server.is_empty() {
            self.to_server
                .flush(self.server.as_fd())
                .map_err(|e| format!("writing: {e}"))?;
            if self.to_server.is_empty() {
                break;
            }
            if Instant::now() > deadline {
                return Err("the compositor stopped reading".to_owned());
            }
            poll(&mut [(self.server.as_fd(), libc::POLLOUT)], 100)
                .map_err(|e| format!("poll: {e}"))?;
        }
        Ok(())
    }

    /// wl_display.sync, then handles events until its callback comes.
    fn roundtrip(&mut self, rp: &mut ReplayState) -> Result<(), String> {
        let cb = self.ids.alloc();
        self.ids
            .temps
            .insert(cb, interfaces::by_name("wl_callback").unwrap());
        rp.done_cb = Some(cb);
        rp.done = false;
        self.to_server.push_msg(Msg::new(1, 0, vec![A::New(cb)]));
        let deadline = Instant::now() + TIMEOUT;
        let mut buf = vec![0u8; 64 * 1024];
        while !rp.done {
            self.flush_server()?;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the compositor didn't answer".to_owned());
            }
            let revents = poll(
                &mut [(self.server.as_fd(), libc::POLLIN)],
                left.as_millis() as i32,
            )
            .map_err(|e| format!("poll: {e}"))?;
            if revents[0] == 0 {
                continue;
            }
            loop {
                let mut fds = Vec::new();
                match recv_with_fds(self.server.as_fd(), &mut buf, &mut fds) {
                    Ok(0) => return Err("the compositor went away".to_owned()),
                    Ok(n) => self.from_server.push(&buf[..n], fds),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(format!("reading: {e}")),
                }
            }
            self.pump_server(Some(rp)).map_err(|e| e.to_string())?;
            if let Some(err) = self.model.error.take() {
                return Err(format!("protocol error: {err}"));
            }
        }
        rp.done_cb = None;
        Ok(())
    }

    /// Whether object `key` can be rebuilt (memoized in `memo`).
    fn want(&self, key: Key, versions: &HashMap<u32, u32>, memo: &mut HashMap<Key, bool>) -> bool {
        if let Some(w) = memo.get(&key) {
            return *w;
        }
        let m = &self.model;
        let o = &m.objs[&key];
        let parent_ok = |memo: &mut HashMap<Key, bool>| {
            o.parent
                .is_some_and(|p| m.objs.contains_key(&p) && self.want(p, versions, memo))
        };
        let w = match &o.creation {
            Creation::Display => true,
            Creation::Request(rec) => {
                let bind_ok = match (o.parent.map(|p| m.objs[&p].iface.name), rec.args.first()) {
                    (Some("wl_registry"), Some(V::Uint(name))) => self
                        .ids
                        .global_c2s(*name)
                        .and_then(|s| versions.get(&s))
                        .is_some_and(|v| o.version <= *v),
                    _ => true,
                };
                bind_ok
                    && m.replayable_iface(key)
                    && m.role_ok(key)
                    && parent_ok(memo)
                    && required_refs_ok(self, rec, o.parent, versions, memo)
            }
            Creation::Event => {
                o.iface.name == "wl_buffer"
                    && parent_ok(memo)
                    && o.parent.is_some_and(|p| {
                        let p = &m.objs[&p];
                        p.iface.name == "zwp_linux_buffer_params_v1"
                            && p.sets.contains_key("create")
                    })
            }
        };
        memo.insert(key, w);
        w
    }

    /// The objects to rebuild, in creation order.
    fn plan(&self, versions: &HashMap<u32, u32>) -> BTreeSet<Key> {
        let mut memo = HashMap::new();
        let mut wanted = BTreeSet::new();
        let mut queue: Vec<Key> = self.model.by_id.values().copied().collect();
        while let Some(k) = queue.pop() {
            if wanted.contains(&k)
                || !self.model.objs.contains_key(&k)
                || !self.want(k, versions, &mut memo)
            {
                continue;
            }
            wanted.insert(k);
            let o = &self.model.objs[&k];
            queue.extend(o.parent);
            queue.extend(recs_of(o).flat_map(|r| r.refs().collect::<Vec<_>>()));
        }
        wanted
    }

    /// Builds a recorded request for the compositor. `target_s`: the compositor id of the
    /// object it goes to; new objects get ids and are paired. None if a required object is
    /// missing.
    fn build(
        &mut self,
        rec: &Rec,
        iface: &'static Interface,
        target_s: u32,
        ks: &mut HashMap<Key, u32>,
    ) -> Option<Msg> {
        let desc = iface.requests.get(rec.opcode as usize)?;
        let mut sig = desc.signature.iter();
        let mut args = Vec::with_capacity(rec.args.len());
        let is_bind = iface.name == "wl_registry" && desc.name == "bind";
        for (i, v) in rec.args.iter().enumerate() {
            let ty = sig.next()?;
            args.push(match v {
                V::Int(x) => A::Int(*x),
                V::Uint(x) if is_bind && i == 0 => A::Uint(self.ids.global_c2s(*x)?),
                V::Uint(x) => A::Uint(*x),
                V::Fixed(x) => A::Fixed(*x),
                V::Str(s) => A::Str(s.clone()),
                V::Array(a) => A::Array(a.clone()),
                V::Fd(fd) => A::Fd(fd.try_clone().ok()?),
                V::Obj(None) => A::Obj(0),
                V::Obj(Some(k)) => match ks.get(k) {
                    Some(s) => A::Obj(*s),
                    None if matches!(ty, ArgumentType::Object(AllowNull::Yes)) => A::Obj(0),
                    None => return None,
                },
                V::New(k) => {
                    let s = self.ids.alloc();
                    ks.insert(*k, s);
                    let o = self.model.objs.get_mut(k).unwrap();
                    o.rebuilt = true;
                    if o.alive {
                        self.ids.pair(o.id, s);
                    } else {
                        self.ids.temps.insert(s, o.iface);
                    }
                    A::New(s)
                }
            });
        }
        Some(Msg::new(target_s, rec.opcode, args))
    }

    /// Rebuilds this app's objects in the (new) compositor connected as `self.server`. Returns
    /// how many live objects came back.
    pub(super) fn replay(&mut self) -> Result<usize, String> {
        let mut rp = ReplayState {
            pre_globals: self.model.globals.keys().copied().collect(),
            ..Default::default()
        };

        // 1. Globals.
        let reg = self.ids.alloc();
        self.ids
            .temps
            .insert(reg, interfaces::by_name("wl_registry").unwrap());
        self.to_server.push_msg(Msg::new(1, 1, vec![A::New(reg)]));
        rp.collect = Some((reg, Vec::new()));
        self.roundtrip(&mut rp)?;
        let server_globals = rp.collect.take().unwrap().1;
        let mut versions = HashMap::new();
        let mut used = HashSet::new();
        let mut removed = Vec::new();
        for (&c, (iface, _)) in self.model.globals.clone().iter() {
            match server_globals
                .iter()
                .find(|(s, i, _)| !used.contains(s) && i == iface)
            {
                Some((s, _, v)) => {
                    used.insert(*s);
                    versions.insert(*s, *v);
                    self.ids.pair_global(c, *s);
                }
                None => removed.push(c),
            }
        }
        let registries: Vec<u32> = self
            .model
            .objs
            .values()
            .filter(|o| o.alive && o.iface.name == "wl_registry")
            .map(|o| o.id)
            .collect();
        for c in removed {
            self.model.globals.remove(&c);
            rp.pre_globals.remove(&c);
            for &r in &registries {
                self.to_client.push_msg(Msg::new(r, 1, vec![A::Uint(c)]));
            }
        }
        rp.replayed_registries = registries.into_iter().collect();

        // 2. Objects and their state.
        let wanted = self.plan(&versions);
        let mut ks: HashMap<Key, u32> = HashMap::new();
        let mut deferred: Vec<(Key, Rec)> = Vec::new();
        for &k in &wanted {
            let o = &self.model.objs[&k];
            let iface = o.iface;
            let msg = match o.creation.clone() {
                Creation::Display => {
                    ks.insert(k, 1);
                    continue;
                }
                Creation::Request(rec) => {
                    let parent = o.parent.expect("created by a request on a parent");
                    let (Some(&ps), piface) = (ks.get(&parent), self.model.objs[&parent].iface)
                    else {
                        continue;
                    };
                    self.build(&rec, piface, ps, &mut ks).map(|m| (m, piface))
                }
                Creation::Event => {
                    // A dmabuf buffer made by zwp_linux_buffer_params.create: create_immed.
                    let parent = o.parent.unwrap();
                    let p = &self.model.objs[&parent];
                    let (Some(&ps), Some(create), Some(op)) = (
                        ks.get(&parent),
                        p.sets.get("create").cloned(),
                        request_by_name(p.iface, "create_immed"),
                    ) else {
                        continue;
                    };
                    let mut args = vec![V::New(k)];
                    args.extend(create.args);
                    let piface = p.iface;
                    self.build(&Rec { opcode: op, args }, piface, ps, &mut ks)
                        .map(|m| (m, piface))
                }
            };
            let Some((mut msg, target_iface)) = msg else {
                debug!(
                    "wl-proxy: {}: couldn't rebuild {}#{}",
                    self.label, iface.name, self.model.objs[&k].id
                );
                continue;
            };
            if target_iface.name == "zxdg_importer_v2" {
                // An import by a handle from an earlier compositor.
                self.translate_import(&mut msg);
            }
            self.send_replay(msg, target_iface);
            let Some(&s) = ks.get(&k) else { continue };

            // The compositor's text-input `done` serial counts commits; catch it up with the
            // app's count (an empty commit on a fresh text input changes nothing).
            if iface.name == "zwp_text_input_v3" {
                let n = self.model.objs[&k].commits.min(1 << 20);
                if let Some(op) = request_by_name(iface, "commit") {
                    for _ in 0..n {
                        self.to_server.push_msg(Msg::new(s, op, vec![]));
                    }
                }
            }

            let o = &self.model.objs[&k];
            let recs: Vec<Rec> = o
                .sets
                .iter()
                .filter(|(g, _)| {
                    !(iface.name == "wl_surface" && matches!(**g, "attach" | "offset"))
                        // Sent once the new compositor's buffer details came (redo_screen_copy).
                        && !(iface.name == "zwlr_screencopy_frame_v1" && **g == "copy")
                })
                .map(|(_, r)| r.clone())
                .chain(o.accum.iter().cloned())
                .collect();
            // Objects that others are built from get their content right away; everything
            // else after all objects exist (an xdg_surface takes no state before its role).
            let builder = matches!(
                iface.name,
                "wl_region" | "wl_shm_pool" | "zwp_linux_buffer_params_v1" | "xdg_positioner"
            );
            for rec in recs {
                if builder && rec.refs().all(|r| ks.contains_key(&r)) {
                    if let Some(m) = self.build(&rec, iface, s, &mut ks) {
                        self.send_replay(m, iface);
                    }
                } else {
                    deferred.push((k, rec));
                }
            }
        }
        for (k, rec) in deferred {
            let (Some(&s), iface) = (ks.get(&k), self.model.objs[&k].iface) else {
                continue;
            };
            // Requests about objects that weren't rebuilt are left out (or sent with null).
            if let Some(m) = self.build(&rec, iface, s, &mut ks) {
                self.send_replay(m, iface);
            }
        }

        // Activation tokens the app is still waiting for: asked for again (their state is set).
        let tokens: Vec<Key> = wanted
            .iter()
            .filter(|k| {
                let o = &self.model.objs[*k];
                o.alive && o.awaiting && ks.contains_key(*k)
            })
            .copied()
            .collect();
        for k in tokens {
            let iface = self.model.objs[&k].iface;
            if let Some(op) = request_by_name(iface, "commit") {
                self.send_replay(Msg::new(ks[&k], op, vec![]), iface);
            }
        }

        // 3. Toplevels: the first commit without a buffer, and the configure that answers it.
        let live = |c: &Connection, k: &Key| c.model.objs[k].alive && ks.contains_key(k);
        let surfaces: Vec<Key> = wanted
            .iter()
            .filter(|k| live(self, k) && self.model.objs[*k].iface.name == "wl_surface")
            .copied()
            .collect();
        // Surface -> its xdg_surface (rebuilt or not).
        let mut role_of: HashMap<Key, Key> = HashMap::new();
        // Subsurface tree: surface -> parent surface.
        let mut parent_of: HashMap<Key, Key> = HashMap::new();
        for (k, o) in &self.model.objs {
            if !o.alive {
                continue;
            }
            let Creation::Request(rec) = &o.creation else {
                continue;
            };
            match o.iface.name {
                name if super::model::CONFIGURED_ROLES.contains(&name) => {
                    if let Some(V::Obj(Some(surface))) = rec.args.get(1) {
                        role_of.insert(*surface, *k);
                    }
                }
                "wl_subsurface" if ks.contains_key(k) => {
                    if let (Some(V::Obj(Some(child))), Some(V::Obj(Some(parent)))) =
                        (rec.args.get(1), rec.args.get(2))
                    {
                        parent_of.insert(*child, *parent);
                    }
                }
                _ => (),
            }
        }
        let surface_iface = interfaces::by_name("wl_surface").unwrap();
        let commit = request_by_name(surface_iface, "commit").unwrap();
        for k in &surfaces {
            if role_of.get(k).is_some_and(|x| ks.contains_key(x)) {
                self.send_replay(Msg::new(ks[k], commit, vec![]), surface_iface);
            }
        }
        rp.auto_ack = true;
        self.roundtrip(&mut rp)?;

        // 4. Buffers back up, children before parents. Surfaces whose role wasn't rebuilt
        // (popups) stay unmapped.
        let depth = |mut k: Key| {
            let mut d = 0;
            while let Some(p) = parent_of.get(&k) {
                d += 1;
                k = *p;
                if d > 64 {
                    break;
                }
            }
            d
        };
        let mut order: Vec<Key> = surfaces
            .iter()
            .filter(|k| role_of.get(*k).is_none_or(|x| ks.contains_key(x)))
            .copied()
            .collect();
        order.sort_by_key(|k| std::cmp::Reverse(depth(*k)));
        for k in order {
            let s = ks[&k];
            let o = &self.model.objs[&k];
            let version = o.version;
            let attach = o.sets.get("attach").cloned();
            let offset = o.sets.get("offset").cloned();
            if let Some(rec) = attach {
                if let Some(V::Obj(Some(b))) = rec.args.first() {
                    let b = *b;
                    if let Some(m) = self.build(&rec, surface_iface, s, &mut ks) {
                        if ks.contains_key(&b) {
                            self.send_replay(m, surface_iface);
                            if let Some(buf) = self.model.objs.get_mut(&b) {
                                buf.busy = true;
                            }
                        }
                    }
                }
            }
            if let Some(m) = offset.and_then(|r| self.build(&r, surface_iface, s, &mut ks)) {
                self.send_replay(m, surface_iface);
            }
            let full = vec![A::Int(0), A::Int(0), A::Int(i32::MAX), A::Int(i32::MAX)];
            let damage = if version >= 4 {
                "damage_buffer"
            } else {
                "damage"
            };
            let op = request_by_name(surface_iface, damage).unwrap();
            self.send_replay(Msg::new(s, op, full), surface_iface);
            self.send_replay(Msg::new(s, commit, vec![]), surface_iface);
        }

        // 5. Objects only the proxy needed.
        for k in wanted.iter().rev() {
            let o = &self.model.objs[k];
            if o.alive {
                continue;
            }
            let Some(&s) = ks.get(k) else { continue };
            let destroy = o
                .iface
                .requests
                .iter()
                .position(|d| d.is_destructor && d.signature.is_empty());
            if let Some(op) = destroy {
                let iface = o.iface;
                self.send_replay(Msg::new(s, op as u16, vec![]), iface);
            }
        }
        self.roundtrip(&mut rp)?;

        let mut count = 0;
        for k in &wanted {
            if let Some(o) = self.model.objs.get_mut(k) {
                if o.alive && ks.contains_key(k) {
                    o.on_server = true;
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

fn recs_of(o: &super::model::Obj) -> impl Iterator<Item = &Rec> {
    let creation = match &o.creation {
        Creation::Request(r) => Some(r),
        _ => None,
    };
    creation.into_iter().chain(o.sets.values()).chain(&o.accum)
}

/// A creation request's non-nullable object arguments must be rebuildable.
fn required_refs_ok(
    c: &Connection,
    rec: &Rec,
    parent: Option<Key>,
    versions: &HashMap<u32, u32>,
    memo: &mut HashMap<Key, bool>,
) -> bool {
    let Some(parent) = parent else { return true };
    let piface = c.model.objs[&parent].iface;
    let Some(desc) = piface.requests.get(rec.opcode as usize) else {
        return false;
    };
    rec.args
        .iter()
        .zip(desc.signature)
        .all(|(v, ty)| match (v, ty) {
            (V::Obj(Some(k)), ArgumentType::Object(AllowNull::No)) => {
                c.model.objs.contains_key(k) && c.want(*k, versions, memo)
            }
            (V::Obj(None), ArgumentType::Object(AllowNull::No)) => false,
            _ => true,
        })
}

//! Clipboard that survives the app that copied (like Plasma's Klipper).
//!
//! Whenever a client sets the clipboard or primary selection, arbor reads every offered format in
//! the background (size-capped, `clipboard { max-size-mb }`). When that selection disappears
//! because its owner went away (the client quit or destroyed its source without another client
//! taking over), arbor offers the copy as a compositor-owned selection. Selections marked
//! `x-kde-passwordManagerHint` (password managers) are never kept.
//!
//! Clipboard managers on data-control (Vicinae, cliphist, ...) keep working: arbor never takes
//! the selection while a client owns one, so it doesn't compete with them; they see the restored
//! selection as one more change with the same content.

use std::collections::HashSet;
use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::rustix;
use smithay::wayland::selection::data_device::{
    current_data_device_selection_userdata, request_data_device_client_selection,
    set_data_device_selection, SelectionRequestError,
};
use smithay::wayland::selection::primary_selection::{
    current_primary_selection_userdata, request_primary_client_selection, set_primary_selection,
    SelectionRequestError as PrimarySelectionRequestError,
};
use smithay::wayland::selection::SelectionTarget;

use crate::arbor::State;

/// Password managers mark secrets with this format (KeePassXC, KDE apps).
pub const SECRET_HINT_MIME: &str = "x-kde-passwordManagerHint";
/// Nothing offers this; asking for it tells whether a client selection exists.
const PROBE_MIME: &str = "application/x-arbor-selection-probe";
/// At most this many formats are read from one selection.
const MAX_MIME_TYPES: usize = 48;
/// How long the owner gets to hand over all formats.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Primary changes on every selection drag; wait for it to settle before reading.
const PRIMARY_DEBOUNCE: Duration = Duration::from_millis(400);
const CLIPBOARD_DELAY: Duration = Duration::from_millis(20);
/// How often to check whether the owner of a kept selection is gone.
const PROBE_INTERVAL: Duration = Duration::from_millis(150);

/// Data of a compositor-owned selection: the bytes for each offered format.
#[derive(Debug, Clone)]
pub struct SelectionData(Arc<Vec<(String, Arc<[u8]>)>>);

impl SelectionData {
    pub fn new(entries: Vec<(String, Arc<[u8]>)>) -> Self {
        Self(Arc::new(entries))
    }

    pub fn single(mime_type: &str, data: Arc<[u8]>) -> Self {
        Self::new(vec![(mime_type.to_owned(), data)])
    }

    pub fn get(&self, mime_type: &str) -> Option<Arc<[u8]>> {
        self.0
            .iter()
            .find(|(mime, _)| mime == mime_type)
            .map(|(_, data)| data.clone())
    }

    pub fn mime_types(&self) -> Vec<String> {
        self.0.iter().map(|(mime, _)| mime.clone()).collect()
    }

    pub fn total_len(&self) -> usize {
        self.0.iter().map(|(_, data)| data.len()).sum()
    }
}

#[derive(Debug, Default)]
struct Slot {
    /// Bumped on every client selection change; stale reads are dropped.
    generation: u64,
    /// A client owns the current selection (as far as we know).
    client_owned: bool,
    /// The current selection must not be kept (secret, or too big).
    no_keep: bool,
    /// Copy of the current client selection, once read.
    kept: Option<SelectionData>,
}

#[derive(Debug, Default)]
pub struct ClipboardPersist {
    clipboard: Slot,
    primary: Slot,
    last_probe: Option<Instant>,
}

impl ClipboardPersist {
    fn slot(&mut self, ty: SelectionTarget) -> &mut Slot {
        match ty {
            SelectionTarget::Clipboard => &mut self.clipboard,
            SelectionTarget::Primary => &mut self.primary,
        }
    }

    /// The kept copy of a selection, if any (for tests and IPC).
    pub fn kept(&self, ty: SelectionTarget) -> Option<&SelectionData> {
        match ty {
            SelectionTarget::Clipboard => self.clipboard.kept.as_ref(),
            SelectionTarget::Primary => self.primary.kept.as_ref(),
        }
    }

    fn waiting_for_owner_loss(&self) -> bool {
        [&self.clipboard, &self.primary]
            .iter()
            .any(|slot| slot.client_owned && slot.kept.is_some())
    }
}

/// Which formats to read from an offer, or `None` if it must not be kept.
pub fn formats_to_keep(mime_types: &[String]) -> Option<Vec<String>> {
    if mime_types.iter().any(|m| m == SECRET_HINT_MIME) {
        return None;
    }
    let mut seen = HashSet::new();
    let mut keep: Vec<String> = mime_types
        .iter()
        .filter(|m| !m.is_empty() && seen.insert(m.as_str()))
        .cloned()
        .collect();
    keep.truncate(MAX_MIME_TYPES);
    (!keep.is_empty()).then_some(keep)
}

/// Fills `entries` from `(mime, data)` reads within `budget` bytes total, in offer order;
/// formats that don't fit are left out. Returns `None` if nothing was read.
pub fn assemble(
    reads: Vec<(String, Option<Vec<u8>>)>,
    budget: usize,
) -> Option<Vec<(String, Arc<[u8]>)>> {
    let mut used = 0;
    let mut entries = Vec::new();
    for (mime, data) in reads {
        let Some(data) = data else { continue };
        if used + data.len() > budget {
            continue;
        }
        used += data.len();
        entries.push((mime, Arc::<[u8]>::from(data)));
    }
    (!entries.is_empty()).then_some(entries)
}

/// Reads all pipes concurrently until EOF, the deadline, or `budget` bytes in total.
fn read_pipes(fds: Vec<(String, OwnedFd)>, budget: usize) -> Vec<(String, Option<Vec<u8>>)> {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};

    let deadline = Instant::now() + READ_TIMEOUT;
    let mut bufs: Vec<(String, Vec<u8>, Option<std::fs::File>, bool)> = fds
        .into_iter()
        .map(|(mime, fd)| {
            let _ = rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::NONBLOCK);
            (mime, Vec::new(), Some(std::fs::File::from(fd)), true)
        })
        .collect();
    let mut total = 0usize;

    loop {
        let open: Vec<usize> = (0..bufs.len()).filter(|&i| bufs[i].2.is_some()).collect();
        if open.is_empty() {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            for &i in &open {
                // Unfinished: drop.
                bufs[i].3 = false;
                bufs[i].2 = None;
            }
            break;
        }
        let left = deadline - now;
        let timeout = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut pollfds: Vec<PollFd> = open
            .iter()
            .map(|&i| PollFd::new(bufs[i].2.as_ref().unwrap(), PollFlags::IN))
            .collect();
        if poll(&mut pollfds, Some(&timeout)).is_err() {
            break;
        }
        let ready: Vec<bool> = pollfds.iter().map(|p| !p.revents().is_empty()).collect();
        drop(pollfds);

        for (k, &i) in open.iter().enumerate() {
            if !ready[k] {
                continue;
            }
            let (_, buf, file, ok) = &mut bufs[i];
            let mut chunk = [0u8; 65536];
            match file.as_mut().unwrap().read(&mut chunk) {
                Ok(0) => *file = None,
                Ok(n) => {
                    total += n;
                    if total > budget {
                        // Over the cap: this format is not kept.
                        *ok = false;
                        *file = None;
                        total -= buf.len() + n;
                        buf.clear();
                    } else {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => (),
                Err(_) => {
                    *ok = false;
                    *file = None;
                }
            }
        }
    }

    bufs.into_iter()
        .map(|(mime, buf, _, ok)| (mime, ok.then_some(buf)))
        .collect()
}

impl State {
    /// A client set (or cleared) a selection.
    pub fn clipboard_persist_on_new_selection(
        &mut self,
        ty: SelectionTarget,
        mime_types: Option<Vec<String>>,
    ) {
        let persist = self.arbor.config.borrow().clipboard.persist;
        let slot = self.arbor.clipboard_persist.slot(ty);
        slot.generation += 1;
        slot.kept = None;
        slot.no_keep = false;
        slot.client_owned = mime_types.is_some();

        if !persist {
            return;
        }
        let Some(mime_types) = mime_types else {
            // The owner cleared the selection on purpose.
            return;
        };
        let Some(formats) = formats_to_keep(&mime_types) else {
            slot.no_keep = true;
            return;
        };

        let generation = slot.generation;
        let delay = match ty {
            SelectionTarget::Clipboard => CLIPBOARD_DELAY,
            SelectionTarget::Primary => PRIMARY_DEBOUNCE,
        };
        // The new selection is installed after this callback returns; read it a moment later
        // (primary: once it has settled).
        let _ =
            self.arbor
                .event_loop
                .insert_source(Timer::from_duration(delay), move |_, _, state| {
                    state.clipboard_persist_start_read(ty, generation, formats.clone());
                    TimeoutAction::Drop
                });
    }

    fn clipboard_persist_start_read(
        &mut self,
        ty: SelectionTarget,
        generation: u64,
        formats: Vec<String>,
    ) {
        if self.arbor.clipboard_persist.slot(ty).generation != generation {
            return;
        }

        let seat = self.arbor.seat.clone();
        let mut fds = Vec::new();
        for mime in formats {
            let Ok((read, write)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
            else {
                break;
            };
            let ok = match ty {
                SelectionTarget::Clipboard => {
                    request_data_device_client_selection(&seat, mime.clone(), write).is_ok()
                }
                SelectionTarget::Primary => {
                    request_primary_client_selection(&seat, mime.clone(), write).is_ok()
                }
            };
            if !ok {
                // The selection changed hands or became compositor-owned meanwhile.
                break;
            }
            fds.push((mime, read));
        }
        if fds.is_empty() {
            return;
        }

        let budget = self.arbor.config.borrow().clipboard.max_size_mb as usize * 1024 * 1024;
        let (tx, rx) = calloop::channel::sync_channel(1);
        let res = self
            .arbor
            .event_loop
            .insert_source(rx, move |event, _, state: &mut State| {
                if let calloop::channel::Event::Msg(reads) = event {
                    state.clipboard_persist_read_done(ty, generation, reads, budget);
                }
            });
        if res.is_err() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("clipboard-keep".to_owned())
            .spawn(move || {
                let _ = tx.send(read_pipes(fds, budget));
            });
        if let Err(err) = spawned {
            warn!("error spawning clipboard reader thread: {err:?}");
        }
    }

    fn clipboard_persist_read_done(
        &mut self,
        ty: SelectionTarget,
        generation: u64,
        reads: Vec<(String, Option<Vec<u8>>)>,
        budget: usize,
    ) {
        let slot = self.arbor.clipboard_persist.slot(ty);
        if slot.generation != generation || slot.no_keep {
            return;
        }
        match assemble(reads, budget) {
            Some(entries) => {
                trace!("kept {ty:?} selection: {} formats", entries.len());
                slot.kept = Some(SelectionData::new(entries));
            }
            None => slot.no_keep = true,
        }
        // The owner may already be gone.
        self.arbor.clipboard_persist.last_probe = None;
        self.clipboard_persist_check_owners();
    }

    /// Called every event loop iteration: if the owner of a kept selection went away, offer the
    /// kept copy instead.
    pub fn clipboard_persist_check_owners(&mut self) {
        let persist = &mut self.arbor.clipboard_persist;
        if !persist.waiting_for_owner_loss() {
            return;
        }
        let now = Instant::now();
        if persist
            .last_probe
            .is_some_and(|last| now.duration_since(last) < PROBE_INTERVAL)
        {
            return;
        }
        persist.last_probe = Some(now);

        let disable_primary = self.arbor.config.borrow().clipboard.disable_primary;
        for ty in [SelectionTarget::Clipboard, SelectionTarget::Primary] {
            if ty == SelectionTarget::Primary && disable_primary {
                continue;
            }
            let slot = self.arbor.clipboard_persist.slot(ty);
            if !slot.client_owned || slot.kept.is_none() {
                continue;
            }
            let Some(gone) = self.selection_owner_gone(ty) else {
                continue;
            };
            let slot = self.arbor.clipboard_persist.slot(ty);
            if !gone {
                continue;
            }
            slot.client_owned = false;
            let data = slot.kept.clone().unwrap();
            debug!("selection owner gone; keeping the {ty:?} selection");
            let dh = self.arbor.display_handle.clone();
            let seat = self.arbor.seat.clone();
            match ty {
                SelectionTarget::Clipboard => {
                    set_data_device_selection(&dh, &seat, data.mime_types(), data)
                }
                SelectionTarget::Primary => {
                    set_primary_selection(&dh, &seat, data.mime_types(), data)
                }
            }
        }
    }

    /// Whether the selection is now empty (`Some(true)`), still client-owned (`Some(false)`);
    /// `None` when it's compositor-owned (someone else took over).
    fn selection_owner_gone(&mut self, ty: SelectionTarget) -> Option<bool> {
        let compositor_owned = match ty {
            SelectionTarget::Clipboard => {
                current_data_device_selection_userdata(&self.arbor.seat).is_some()
            }
            SelectionTarget::Primary => {
                current_primary_selection_userdata(&self.arbor.seat).is_some()
            }
        };
        if compositor_owned {
            self.arbor.clipboard_persist.slot(ty).client_owned = false;
            return None;
        }

        let Ok(probe) = rustix::fs::open(
            "/dev/null",
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) else {
            return None;
        };
        let seat = &self.arbor.seat;
        match ty {
            SelectionTarget::Clipboard => {
                match request_data_device_client_selection(seat, PROBE_MIME.to_owned(), probe) {
                    Err(SelectionRequestError::NoSelection) => Some(true),
                    Err(SelectionRequestError::InvalidMimetype) => Some(false),
                    // Something offering our probe type is never ours to keep.
                    Ok(()) => Some(false),
                    Err(SelectionRequestError::ServerSideSelection) => {
                        self.arbor.clipboard_persist.slot(ty).client_owned = false;
                        None
                    }
                }
            }
            SelectionTarget::Primary => {
                match request_primary_client_selection(seat, PROBE_MIME.to_owned(), probe) {
                    Err(PrimarySelectionRequestError::NoSelection) => Some(true),
                    Err(PrimarySelectionRequestError::InvalidMimetype) => Some(false),
                    Ok(()) => Some(false),
                    Err(PrimarySelectionRequestError::ServerSideSelection) => {
                        self.arbor.clipboard_persist.slot(ty).client_owned = false;
                        None
                    }
                }
            }
        }
    }
}

/// Writes `data` to a selection target fd off the main thread.
pub fn write_selection(fd: OwnedFd, data: Arc<[u8]>) {
    std::thread::spawn(move || {
        use std::io::Write as _;
        // Clear O_NONBLOCK, otherwise write_all() stops halfway.
        if let Err(err) = rustix::fs::fcntl_setfl(fd.as_fd(), rustix::fs::OFlags::empty()) {
            warn!("error clearing flags on selection target fd: {err:?}");
        }
        if let Err(err) = std::fs::File::from(fd).write_all(&data) {
            warn!("error writing selection: {err:?}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn secret_selections_are_not_kept() {
        assert_eq!(formats_to_keep(&s(&["text/plain", SECRET_HINT_MIME])), None);
        assert_eq!(
            formats_to_keep(&s(&["text/plain", "text/plain", "UTF8_STRING"])),
            Some(s(&["text/plain", "UTF8_STRING"]))
        );
        assert_eq!(formats_to_keep(&[]), None);
    }

    #[test]
    fn assemble_respects_budget() {
        let reads = vec![
            ("a".to_owned(), Some(vec![0; 10])),
            ("b".to_owned(), Some(vec![0; 100])),
            ("c".to_owned(), None),
            ("d".to_owned(), Some(vec![0; 5])),
        ];
        let got = assemble(reads, 20).unwrap();
        let names: Vec<_> = got.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(names, ["a", "d"]);
        assert_eq!(assemble(vec![("x".to_owned(), Some(vec![0; 5]))], 1), None);
    }

    #[test]
    fn read_pipes_reads_concurrently_and_caps() {
        use std::io::Write as _;
        let (r1, w1) = rustix::pipe::pipe().unwrap();
        let (r2, w2) = rustix::pipe::pipe().unwrap();
        let (r3, w3) = rustix::pipe::pipe().unwrap();
        // Writers that block on a full pipe unless the reader drains them concurrently.
        let t = std::thread::spawn(move || {
            let mut w2 = std::fs::File::from(w2);
            let mut w1 = std::fs::File::from(w1);
            w2.write_all(&vec![2u8; 200_000]).unwrap();
            drop(w2);
            w1.write_all(b"hello").unwrap();
            drop(w1);
            let mut w3 = std::fs::File::from(w3);
            let _ = w3.write_all(&vec![3u8; 2_000_000]);
        });
        let out = read_pipes(
            vec![
                ("one".to_owned(), r1),
                ("two".to_owned(), r2),
                ("three".to_owned(), r3),
            ],
            1_000_000,
        );
        t.join().unwrap();
        assert_eq!(out[0].1.as_deref(), Some(&b"hello"[..]));
        assert_eq!(out[1].1.as_ref().map(Vec::len), Some(200_000));
        assert_eq!(out[2].1, None, "over budget");
    }

    #[test]
    fn selection_data_lookup() {
        let data = SelectionData::new(vec![
            ("text/plain".to_owned(), Arc::from(&b"hi"[..])),
            ("text/html".to_owned(), Arc::from(&b"<b>hi</b>"[..])),
        ]);
        assert_eq!(data.get("text/html").as_deref(), Some(&b"<b>hi</b>"[..]));
        assert_eq!(data.get("image/png"), None);
        assert_eq!(data.mime_types(), ["text/plain", "text/html"]);
        assert_eq!(data.total_len(), 11);
    }
}

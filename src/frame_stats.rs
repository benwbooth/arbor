//! arbor: cheap frame statistics for diagnosing slow frames on the real display.
//!
//! Off by default. Turn on at runtime with `touch $XDG_RUNTIME_DIR/arbor-frame-stats` and off by
//! removing the file (checked once a second), or start arbor with `ARBOR_FRAME_STATS=1`. While on,
//! arbor logs one line per output per second (`journalctl --user -u arbor -f | grep 'frame
//! stats'`):
//!
//! - `renders`: frames rendered (`empty`: nothing changed, nothing queued),
//! - `cpu`: time to build the element list and hand it to the GPU (avg/max), not GPU time,
//! - `presented`: frames that reached the screen and the largest gap between two of them while
//!   frames came continuously; `late` counts gaps over 1.5 refresh intervals (dropped frames),
//! - `refresh` and `vrr`: the frame clock's interval and whether adaptive sync is on,
//! - `elements`: the largest element count in a frame,
//! - `glass`: liquid glass backdrop captures and the pixels they copied and blurred,
//! - `overview drawing`: labels and thumbnails rasterized for the overview (on a background thread)
//!   and the time that took.
//!
//! All recording happens on the main thread; when off, each hook costs one boolean check.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Gaps longer than this are idle time, not dropped frames.
const IDLE_GAP: Duration = Duration::from_millis(250);

#[derive(Default)]
struct OutputStats {
    renders: u32,
    empty: u32,
    cpu_total: Duration,
    cpu_max: Duration,
    elements_max: usize,
    presents: u32,
    last_present: Option<Instant>,
    gap_max: Duration,
    late: u32,
    refresh: Option<Duration>,
    vrr: bool,
}

#[derive(Default)]
struct Totals {
    cairo_count: u32,
    cairo_total: Duration,
    cairo_max: Duration,
    glass_captures: u32,
    glass_px: u64,
}

struct State {
    env: bool,
    flag: Option<PathBuf>,
    enabled: bool,
    last_check: Option<Instant>,
    last_flush: Option<Instant>,
    outputs: BTreeMap<String, OutputStats>,
    totals: Totals,
}

impl State {
    fn new() -> Self {
        Self {
            env: std::env::var_os("ARBOR_FRAME_STATS").is_some_and(|v| !v.is_empty() && v != "0"),
            flag: std::env::var_os("XDG_RUNTIME_DIR")
                .map(|dir| PathBuf::from(dir).join("arbor-frame-stats")),
            enabled: false,
            last_check: None,
            last_flush: None,
            outputs: BTreeMap::new(),
            totals: Totals::default(),
        }
    }

    fn refresh_enabled(&mut self, now: Instant) {
        if self
            .last_check
            .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        self.last_check = Some(now);
        let was = self.enabled;
        self.enabled = self.env || self.flag.as_ref().is_some_and(|f| f.exists());
        if self.enabled != was {
            info!("frame stats {}", if self.enabled { "on" } else { "off" });
            self.outputs.clear();
            self.totals = Totals::default();
            self.last_flush = Some(now);
        }
    }

    fn maybe_flush(&mut self, now: Instant) {
        let last = *self.last_flush.get_or_insert(now);
        let elapsed = now.saturating_duration_since(last);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        self.last_flush = Some(now);
        let secs = elapsed.as_secs_f64();

        let t = std::mem::take(&mut self.totals);
        for (name, s) in &mut self.outputs {
            if s.renders == 0 && s.presents == 0 {
                continue;
            }
            let ms = |d: Duration| d.as_secs_f64() * 1000.;
            let avg = if s.renders > 0 {
                s.cpu_total / s.renders
            } else {
                Duration::ZERO
            };
            let refresh = s
                .refresh
                .map_or_else(|| "?".to_owned(), |r| format!("{:.2}ms", ms(r)));
            info!(
                "frame stats {name}: {:.0} renders/s ({} empty), cpu avg {:.2}ms max {:.2}ms, \
                 {:.0} presented/s, max gap {:.2}ms, {} late, refresh {refresh}, vrr {}, \
                 {} elements max",
                f64::from(s.renders) / secs,
                s.empty,
                ms(avg),
                ms(s.cpu_max),
                f64::from(s.presents) / secs,
                ms(s.gap_max),
                s.late,
                if s.vrr { "on" } else { "off" },
                s.elements_max,
            );
            let last_present = s.last_present;
            *s = OutputStats {
                last_present,
                ..OutputStats::default()
            };
        }
        if t.glass_captures > 0 || t.cairo_count > 0 {
            info!(
                "frame stats: glass {} captures ({:.1} Mpx), overview drawing {} textures \
                 {:.2}ms (max {:.2}ms)",
                t.glass_captures,
                t.glass_px as f64 / 1e6,
                t.cairo_count,
                t.cairo_total.as_secs_f64() * 1000.,
                t.cairo_max.as_secs_f64() * 1000.,
            );
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

/// Whether frame stats are being recorded.
pub fn enabled() -> bool {
    STATE.with_borrow_mut(|s| {
        s.refresh_enabled(Instant::now());
        s.enabled
    })
}

fn with(f: impl FnOnce(&mut State, Instant)) {
    STATE.with_borrow_mut(|s| {
        let now = Instant::now();
        s.refresh_enabled(now);
        if s.enabled {
            f(s, now);
            s.maybe_flush(now);
        }
    });
}

/// A frame was rendered for `output`; `cpu` is the time from starting to build the elements to
/// handing them to the GPU.
pub fn rendered(output: &str, cpu: Duration, elements: usize, empty: bool) {
    with(|s, _| {
        let o = s.outputs.entry(output.to_owned()).or_default();
        o.renders += 1;
        if empty {
            o.empty += 1;
        }
        o.cpu_total += cpu;
        o.cpu_max = o.cpu_max.max(cpu);
        o.elements_max = o.elements_max.max(elements);
    });
}

/// A frame reached the screen of `output`.
pub fn presented(output: &str, refresh: Option<Duration>, vrr: bool) {
    with(|s, now| {
        let o = s.outputs.entry(output.to_owned()).or_default();
        o.presents += 1;
        o.refresh = refresh;
        o.vrr = vrr;
        if let Some(last) = o.last_present {
            let gap = now.saturating_duration_since(last);
            if gap < IDLE_GAP {
                o.gap_max = o.gap_max.max(gap);
                if refresh.is_some_and(|r| gap > r.mul_f64(1.5)) {
                    o.late += 1;
                }
            }
        }
        o.last_present = Some(now);
    });
}

/// Time a background thread spent rasterizing one overview texture.
pub fn overview_drawing(d: Duration) {
    with(|s, _| {
        s.totals.cairo_count += 1;
        s.totals.cairo_total += d;
        s.totals.cairo_max = s.totals.cairo_max.max(d);
    });
}

/// A liquid glass backdrop capture of `px` pixels.
pub fn glass_capture(px: u64) {
    with(|s, _| {
        s.totals.glass_captures += 1;
        s.totals.glass_px += px;
    });
}

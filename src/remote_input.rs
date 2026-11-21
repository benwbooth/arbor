//! Input injected by remote-desktop clients (Mutter RemoteDesktop over D-Bus, and EIS).
//!
//! Remote events go through the same pipeline as real devices (`State::process_input_event`)
//! with a synthetic [`RemoteInputBackend`], so binds, global shortcuts, focus-follows-mouse and
//! idle handling all see them exactly like hardware input.
//!
//! Keysyms are turned into key presses on the current keymap: the key (and the modifier keys
//! for its shift level) that produce the keysym in the active layout, taking locked modifiers
//! such as Caps Lock into account. A keysym that no key produces (e.g. "—" or "é" on a US
//! layout) is typed through a temporary keymap with one spare key mapped to it; the original
//! keymap (and modifier state) comes back right after the key is released.

use std::collections::{HashMap, HashSet};

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Device,
    DeviceCapability, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, Keycode,
    PointerAxisEvent, PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent,
    TouchDownEvent, TouchEvent, TouchFrameEvent, TouchMotionEvent, TouchSlot, TouchUpEvent,
    UnusedEvent,
};
use smithay::input::keyboard::{xkb, Keysym};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle};

use crate::arbor::State;
use crate::utils::get_monotonic_time;

/// Offset between evdev key codes (what RemoteDesktop and EIS send) and XKB key codes.
const EVDEV_OFFSET: u32 = 8;

/// Where an absolute position (pointer or touch) is relative to.
#[derive(Debug, Clone, PartialEq)]
pub enum AbsTarget {
    /// A monitor stream: logical coordinates within that output.
    Output(String),
    /// A window stream: logical coordinates within that window.
    Window(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}

/// One remote input event, in Mutter RemoteDesktop terms.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteInput {
    /// evdev key code.
    Key {
        keycode: u32,
        pressed: bool,
    },
    Keysym {
        keysym: u32,
        pressed: bool,
    },
    PointerMotion {
        dx: f64,
        dy: f64,
    },
    PointerMotionAbsolute {
        target: AbsTarget,
        x: f64,
        y: f64,
    },
    /// evdev button code (BTN_LEFT = 0x110, ...).
    PointerButton {
        button: u32,
        pressed: bool,
    },
    /// Smooth scroll in logical pixels. `finish` marks the end of a finger scroll.
    PointerAxis {
        dx: f64,
        dy: f64,
        source: ScrollSource,
        finish: bool,
    },
    /// Wheel clicks; `horizontal` picks the axis.
    PointerAxisDiscrete {
        horizontal: bool,
        steps: i32,
    },
    TouchDown {
        target: AbsTarget,
        slot: u32,
        x: f64,
        y: f64,
    },
    TouchMotion {
        target: AbsTarget,
        slot: u32,
        x: f64,
        y: f64,
    },
    TouchUp {
        slot: u32,
    },
}

/// Identifies who injects: a RemoteDesktop session or an EIS client.
pub type RemoteSourceId = u64;

/// What a keysym press turned into, so that its release undoes exactly that.
#[derive(Debug, Clone)]
struct KeysymPress {
    keycode: Keycode,
    /// Modifier keys pressed for the key's shift level, released after the key.
    mods: Vec<Keycode>,
    /// The keymap to go back to after the release, when the key came from a temporary keymap.
    restore_keymap: Option<String>,
}

#[derive(Debug, Default)]
struct SourceState {
    keys: HashSet<Keycode>,
    keysyms: HashMap<u32, KeysymPress>,
    buttons: HashSet<u32>,
    touches: HashSet<u32>,
}

/// Compositor-side state of all remote input sources.
#[derive(Debug, Default)]
pub struct RemoteInputState {
    sources: HashMap<RemoteSourceId, SourceState>,
    /// Key codes currently held by any remote source (for `KeyboardKeyEvent::count`).
    held: HashSet<Keycode>,
    /// A temporary keymap is installed for an unmapped keysym.
    temp_keymap_active: bool,
}

// ---------------------------------------------------------------------------------------------
// Synthetic input backend.

pub struct RemoteInputBackend;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteDevice {
    /// Output that absolute positions of this event are relative to.
    output: Option<Output>,
}

impl RemoteDevice {
    pub fn output(&self) -> Option<&Output> {
        self.output.as_ref()
    }
}

impl Device for RemoteDevice {
    fn id(&self) -> String {
        String::from("arbor-remote-input")
    }

    fn name(&self) -> String {
        String::from("arbor remote input")
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(
            capability,
            DeviceCapability::Keyboard | DeviceCapability::Pointer | DeviceCapability::Touch
        )
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

fn now_us() -> u64 {
    get_monotonic_time().as_micros() as u64
}

pub struct RemoteKeyEvent {
    keycode: Keycode,
    state: KeyState,
    count: u32,
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteKeyEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        RemoteDevice { output: None }
    }
}

impl KeyboardKeyEvent<RemoteInputBackend> for RemoteKeyEvent {
    fn key_code(&self) -> Keycode {
        self.keycode
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        self.count
    }
}

pub struct RemoteMotionEvent {
    delta: Point<f64, Logical>,
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteMotionEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        RemoteDevice { output: None }
    }
}

impl PointerMotionEvent<RemoteInputBackend> for RemoteMotionEvent {
    fn delta_x(&self) -> f64 {
        self.delta.x
    }

    fn delta_y(&self) -> f64 {
        self.delta.y
    }

    fn delta_x_unaccel(&self) -> f64 {
        self.delta.x
    }

    fn delta_y_unaccel(&self) -> f64 {
        self.delta.y
    }
}

/// An absolute position on `device.output`, already in that output's untransformed logical
/// coordinates (what `position_transformed` returns; the requested size is ignored).
pub struct RemoteAbsEvent {
    device: RemoteDevice,
    pos: Point<f64, Logical>,
    slot: u32,
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteAbsEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        self.device.clone()
    }
}

impl AbsolutePositionEvent<RemoteInputBackend> for RemoteAbsEvent {
    fn x(&self) -> f64 {
        self.pos.x
    }

    fn y(&self) -> f64 {
        self.pos.y
    }

    fn x_transformed(&self, _width: i32) -> f64 {
        self.pos.x
    }

    fn y_transformed(&self, _height: i32) -> f64 {
        self.pos.y
    }
}

impl PointerMotionAbsoluteEvent<RemoteInputBackend> for RemoteAbsEvent {}

impl TouchEvent<RemoteInputBackend> for RemoteAbsEvent {
    fn slot(&self) -> TouchSlot {
        TouchSlot::from(Some(self.slot))
    }
}

impl TouchDownEvent<RemoteInputBackend> for RemoteAbsEvent {}
impl TouchMotionEvent<RemoteInputBackend> for RemoteAbsEvent {}

pub struct RemoteTouchUpEvent {
    slot: u32,
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteTouchUpEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        RemoteDevice { output: None }
    }
}

impl TouchEvent<RemoteInputBackend> for RemoteTouchUpEvent {
    fn slot(&self) -> TouchSlot {
        TouchSlot::from(Some(self.slot))
    }
}

impl TouchUpEvent<RemoteInputBackend> for RemoteTouchUpEvent {}

pub struct RemoteTouchFrameEvent {
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteTouchFrameEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        RemoteDevice { output: None }
    }
}

impl TouchFrameEvent<RemoteInputBackend> for RemoteTouchFrameEvent {}

pub struct RemoteButtonEvent {
    button: u32,
    state: ButtonState,
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteButtonEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        RemoteDevice { output: None }
    }
}

impl PointerButtonEvent<RemoteInputBackend> for RemoteButtonEvent {
    fn button_code(&self) -> u32 {
        self.button
    }

    fn state(&self) -> ButtonState {
        self.state
    }
}

pub struct RemoteAxisEvent {
    /// (horizontal, vertical)
    amount: (Option<f64>, Option<f64>),
    v120: (Option<f64>, Option<f64>),
    source: AxisSource,
    time: u64,
}

impl Event<RemoteInputBackend> for RemoteAxisEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> RemoteDevice {
        RemoteDevice { output: None }
    }
}

impl PointerAxisEvent<RemoteInputBackend> for RemoteAxisEvent {
    fn amount(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Horizontal => self.amount.0,
            Axis::Vertical => self.amount.1,
        }
    }

    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Horizontal => self.v120.0,
            Axis::Vertical => self.v120.1,
        }
    }

    fn source(&self) -> AxisSource {
        self.source
    }

    fn relative_direction(&self, _axis: Axis) -> AxisRelativeDirection {
        AxisRelativeDirection::Identical
    }
}

impl InputBackend for RemoteInputBackend {
    type Device = RemoteDevice;

    type KeyboardKeyEvent = RemoteKeyEvent;
    type PointerAxisEvent = RemoteAxisEvent;
    type PointerButtonEvent = RemoteButtonEvent;
    type PointerMotionEvent = RemoteMotionEvent;
    type PointerMotionAbsoluteEvent = RemoteAbsEvent;

    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;

    type TouchDownEvent = RemoteAbsEvent;
    type TouchUpEvent = RemoteTouchUpEvent;
    type TouchMotionEvent = RemoteAbsEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = RemoteTouchFrameEvent;

    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;

    type SwitchToggleEvent = UnusedEvent;

    type SpecialEvent = UnusedEvent;
}

// ---------------------------------------------------------------------------------------------
// Keysym → keys.

/// How to type a keysym on a keymap: press `mods` (in order), then `keycode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeysymPlan {
    pub keycode: Keycode,
    pub mods: Vec<Keycode>,
}

/// Modifier keys that select shift levels, in order of preference.
const LEVEL_MODIFIERS: [Keysym; 5] = [
    Keysym::Shift_L,
    Keysym::ISO_Level3_Shift,
    Keysym::ISO_Level5_Shift,
    Keysym::Mode_switch,
    Keysym::Shift_R,
];

/// Finds how to type `keysym` in `layout` of `keymap`, given the currently `locked` modifier
/// mask (Caps Lock, Num Lock). Returns `None` if no key produces it.
///
/// Every key is tried with no extra modifiers first, then with each level modifier key the
/// keymap has, then with pairs of them; the first combination whose resulting keysym (checked
/// on a scratch XKB state with the locked modifiers applied) is `keysym` wins.
pub fn plan_keysym(
    keymap: &xkb::Keymap,
    layout: xkb::LayoutIndex,
    locked: xkb::ModMask,
    keysym: Keysym,
) -> Option<KeysymPlan> {
    if keysym.raw() == 0 {
        return None;
    }

    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw();

    // Level modifier keys present in this layout, with the modifier mask each one sets.
    let mut mod_keys: Vec<Keycode> = Vec::new();
    for wanted in LEVEL_MODIFIERS {
        let found = (min..=max).map(Keycode::new).find(|&kc| {
            keymap.num_levels_for_key(kc, layout) > 0
                && keymap.key_get_syms_by_level(kc, layout, 0) == [wanted]
        });
        if let Some(kc) = found {
            if !mod_keys.contains(&kc) {
                mod_keys.push(kc);
            }
        }
    }

    // Candidate modifier combinations: none, singles, pairs.
    let mut combos: Vec<Vec<Keycode>> = vec![vec![]];
    for &a in &mod_keys {
        combos.push(vec![a]);
    }
    for (i, &a) in mod_keys.iter().enumerate() {
        for &b in &mod_keys[i + 1..] {
            combos.push(vec![a, b]);
        }
    }

    // Keys that have the keysym anywhere in this layout.
    let candidates: Vec<Keycode> = (min..=max)
        .map(Keycode::new)
        .filter(|&kc| {
            (0..keymap.num_levels_for_key(kc, layout))
                .any(|level| keymap.key_get_syms_by_level(kc, layout, level) == [keysym])
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    for combo in &combos {
        let mut state = xkb::State::new(keymap);
        state.update_mask(0, 0, locked, 0, 0, layout);
        for &m in combo {
            state.update_key(m, xkb::KeyDirection::Down);
        }
        for &kc in &candidates {
            if combo.contains(&kc) {
                continue;
            }
            if state.key_get_one_sym(kc) == keysym {
                return Some(KeysymPlan {
                    keycode: kc,
                    mods: combo.clone(),
                });
            }
        }
    }

    None
}

/// A key code with no symbols in any layout, to map an unmapped keysym to. Stays at or below
/// 255 so that X11 clients (Xwayland) can use it too.
pub fn spare_keycode(keymap: &xkb::Keymap) -> Option<Keycode> {
    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw().min(255);
    (min..=max).rev().map(Keycode::new).find(|&kc| {
        keymap.key_get_name(kc).is_some()
            && (0..keymap.num_layouts_for_key(kc)).all(|layout| {
                (0..keymap.num_levels_for_key(kc, layout))
                    .all(|level| keymap.key_get_syms_by_level(kc, layout, level).is_empty())
            })
    })
}

/// `keymap_text` (from `Keymap::get_as_string`) with key `key_name` mapped to `keysym` on its
/// first level. Returns `None` if the text has no `xkb_symbols` section.
pub fn keymap_with_extra_key(keymap_text: &str, key_name: &str, keysym: Keysym) -> Option<String> {
    let start = keymap_text.find("xkb_symbols")?;
    let end = start + keymap_text[start..].find("\n};")?;
    let sym_name = xkb::keysym_get_name(keysym);
    let sym_name = if sym_name.is_empty() {
        format!("0x{:x}", keysym.raw())
    } else {
        sym_name
    };
    let line = format!("\n\tkey <{key_name}> {{ [ {sym_name} ] }};");
    let mut text = String::with_capacity(keymap_text.len() + line.len());
    text.push_str(&keymap_text[..end]);
    text.push_str(&line);
    text.push_str(&keymap_text[end..]);
    Some(text)
}

// ---------------------------------------------------------------------------------------------
// Injection.

impl State {
    /// Injects one remote event from `source`.
    pub fn remote_input(&mut self, source: RemoteSourceId, event: RemoteInput) {
        let time = now_us();
        match event {
            RemoteInput::Key { keycode, pressed } => {
                let keycode = Keycode::new(keycode + EVDEV_OFFSET);
                let src = self.arbor.remote_input.sources.entry(source).or_default();
                // Drop repeats and stray releases so that held-key bookkeeping stays right.
                if pressed == src.keys.contains(&keycode) {
                    return;
                }
                if pressed {
                    src.keys.insert(keycode);
                } else {
                    src.keys.remove(&keycode);
                }
                self.remote_key(keycode, pressed, time);
            }
            RemoteInput::Keysym { keysym, pressed } => {
                if pressed {
                    self.remote_keysym_press(source, keysym, time);
                } else {
                    self.remote_keysym_release(source, keysym, time);
                }
            }
            RemoteInput::PointerMotion { dx, dy } => {
                let event = RemoteMotionEvent {
                    delta: Point::from((dx, dy)),
                    time,
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::PointerMotion { event });
            }
            RemoteInput::PointerMotionAbsolute { target, x, y } => {
                let Some(event) = self.remote_abs_event(&target, x, y, 0, time) else {
                    return;
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::PointerMotionAbsolute {
                    event,
                });
            }
            RemoteInput::PointerButton { button, pressed } => {
                let src = self.arbor.remote_input.sources.entry(source).or_default();
                if pressed == src.buttons.contains(&button) {
                    return;
                }
                if pressed {
                    src.buttons.insert(button);
                } else {
                    src.buttons.remove(&button);
                }
                let event = RemoteButtonEvent {
                    button,
                    state: if pressed {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                    time,
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::PointerButton { event });
            }
            RemoteInput::PointerAxis {
                dx,
                dy,
                source: scroll_source,
                finish,
            } => {
                let source = match scroll_source {
                    ScrollSource::Wheel => AxisSource::Wheel,
                    ScrollSource::Finger => AxisSource::Finger,
                    ScrollSource::Continuous => AxisSource::Continuous,
                };
                // Mutter: 10 px of wheel scroll is one detent.
                let v120 = |d: f64| (source == AxisSource::Wheel).then_some(d / 10. * 120.);
                let event = if finish && dx == 0. && dy == 0. {
                    RemoteAxisEvent {
                        amount: (Some(0.), Some(0.)),
                        v120: (None, None),
                        source: AxisSource::Finger,
                        time,
                    }
                } else {
                    RemoteAxisEvent {
                        amount: ((dx != 0.).then_some(dx), (dy != 0.).then_some(dy)),
                        v120: (
                            (dx != 0.).then(|| v120(dx)).flatten(),
                            (dy != 0.).then(|| v120(dy)).flatten(),
                        ),
                        source,
                        time,
                    }
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::PointerAxis { event });
            }
            RemoteInput::PointerAxisDiscrete { horizontal, steps } => {
                // libinput reports 15 degrees per wheel click.
                let amount = f64::from(steps) * 15.;
                let v120 = f64::from(steps) * 120.;
                let event = if horizontal {
                    RemoteAxisEvent {
                        amount: (Some(amount), None),
                        v120: (Some(v120), None),
                        source: AxisSource::Wheel,
                        time,
                    }
                } else {
                    RemoteAxisEvent {
                        amount: (None, Some(amount)),
                        v120: (None, Some(v120)),
                        source: AxisSource::Wheel,
                        time,
                    }
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::PointerAxis { event });
            }
            RemoteInput::TouchDown { target, slot, x, y } => {
                let src = self.arbor.remote_input.sources.entry(source).or_default();
                if !src.touches.insert(slot) {
                    return;
                }
                let Some(event) = self.remote_abs_event(&target, x, y, slot, time) else {
                    return;
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::TouchDown { event });
                self.remote_touch_frame(time);
            }
            RemoteInput::TouchMotion { target, slot, x, y } => {
                let src = self.arbor.remote_input.sources.entry(source).or_default();
                if !src.touches.contains(&slot) {
                    return;
                }
                let Some(event) = self.remote_abs_event(&target, x, y, slot, time) else {
                    return;
                };
                self.process_input_event(InputEvent::<RemoteInputBackend>::TouchMotion { event });
                self.remote_touch_frame(time);
            }
            RemoteInput::TouchUp { slot } => {
                let src = self.arbor.remote_input.sources.entry(source).or_default();
                if !src.touches.remove(&slot) {
                    return;
                }
                self.remote_touch_up(slot, time);
            }
        }
    }

    /// Releases everything `source` still holds and forgets it.
    pub fn remote_input_source_gone(&mut self, source: RemoteSourceId) {
        let Some(src) = self.arbor.remote_input.sources.remove(&source) else {
            return;
        };
        let time = now_us();

        for (_, press) in src.keysyms {
            self.remote_key(press.keycode, false, time);
            for m in press.mods.iter().rev() {
                self.remote_key(*m, false, time);
            }
            if let Some(keymap) = press.restore_keymap {
                self.remote_restore_keymap(keymap);
            }
        }
        for keycode in src.keys {
            self.remote_key(keycode, false, time);
        }
        for button in src.buttons {
            let event = RemoteButtonEvent {
                button,
                state: ButtonState::Released,
                time,
            };
            self.process_input_event(InputEvent::<RemoteInputBackend>::PointerButton { event });
        }
        for slot in src.touches {
            self.remote_touch_up(slot, time);
        }
    }

    fn remote_key(&mut self, keycode: Keycode, pressed: bool, time: u64) {
        let held = &mut self.arbor.remote_input.held;
        if pressed {
            held.insert(keycode);
        } else {
            held.remove(&keycode);
        }
        let event = RemoteKeyEvent {
            keycode,
            state: if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            count: held.len() as u32,
            time,
        };
        self.process_input_event(InputEvent::<RemoteInputBackend>::Keyboard { event });
    }

    fn remote_touch_up(&mut self, slot: u32, time: u64) {
        let event = RemoteTouchUpEvent { slot, time };
        self.process_input_event(InputEvent::<RemoteInputBackend>::TouchUp { event });
        self.remote_touch_frame(time);
    }

    fn remote_touch_frame(&mut self, time: u64) {
        let event = RemoteTouchFrameEvent { time };
        self.process_input_event(InputEvent::<RemoteInputBackend>::TouchFrame { event });
    }

    fn remote_keysym_press(&mut self, source: RemoteSourceId, keysym: u32, time: u64) {
        if self
            .arbor
            .remote_input
            .sources
            .get(&source)
            .is_some_and(|src| src.keysyms.contains_key(&keysym))
        {
            return;
        }

        let keyboard = self.arbor.seat.get_keyboard().unwrap();
        let mods = keyboard.modifier_state();
        let plan = keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            let layout = xkb.active_layout();
            // SAFETY: the keymap reference does not outlive the lock.
            let keymap = unsafe { xkb.keymap() };
            plan_keysym(
                keymap,
                layout.0,
                mods.serialized.locked,
                Keysym::new(keysym),
            )
        });

        let press = if let Some(plan) = plan {
            let pressed = keyboard.pressed_keys();
            let mods: Vec<Keycode> = plan
                .mods
                .into_iter()
                .filter(|m| !pressed.contains(m))
                .collect();
            for &m in &mods {
                self.remote_key(m, true, time);
            }
            self.remote_key(plan.keycode, true, time);
            KeysymPress {
                keycode: plan.keycode,
                mods,
                restore_keymap: None,
            }
        } else {
            if self.arbor.remote_input.temp_keymap_active {
                debug!("dropping unmapped keysym {keysym:#x}: another one is still held");
                return;
            }
            let Some((original, keycode)) = self.remote_install_temp_keymap(Keysym::new(keysym))
            else {
                debug!("cannot type keysym {keysym:#x}: no key for it and no spare key code");
                return;
            };
            self.arbor.remote_input.temp_keymap_active = true;
            self.remote_key(keycode, true, time);
            KeysymPress {
                keycode,
                mods: Vec::new(),
                restore_keymap: Some(original),
            }
        };

        self.arbor
            .remote_input
            .sources
            .entry(source)
            .or_default()
            .keysyms
            .insert(keysym, press);
    }

    fn remote_keysym_release(&mut self, source: RemoteSourceId, keysym: u32, time: u64) {
        let Some(press) = self
            .arbor
            .remote_input
            .sources
            .get_mut(&source)
            .and_then(|src| src.keysyms.remove(&keysym))
        else {
            return;
        };
        self.remote_key(press.keycode, false, time);
        for m in press.mods.iter().rev() {
            self.remote_key(*m, false, time);
        }
        if let Some(keymap) = press.restore_keymap {
            self.remote_restore_keymap(keymap);
        }
    }

    /// Installs the current keymap plus a spare key mapped to `keysym`. Returns the original
    /// keymap text (to restore later) and the spare key code.
    fn remote_install_temp_keymap(&mut self, keysym: Keysym) -> Option<(String, Keycode)> {
        let keyboard = self.arbor.seat.get_keyboard().unwrap();
        let mods = keyboard.modifier_state();
        let (original, augmented, keycode) = keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            // SAFETY: the keymap reference does not outlive the lock.
            let keymap = unsafe { xkb.keymap() };
            let keycode = spare_keycode(keymap)?;
            let name = keymap.key_get_name(keycode)?.to_owned();
            let original = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
            let augmented = keymap_with_extra_key(&original, &name, keysym)?;
            Some((original, augmented, keycode))
        })?;

        if let Err(err) = keyboard.set_keymap_from_string(self, augmented) {
            warn!(
                "error installing a keymap for keysym {:#x}: {err:?}",
                keysym.raw()
            );
            return None;
        }
        // A new keymap starts from a fresh state: put back locked modifiers and the layout.
        keyboard.set_modifier_state(mods);
        keyboard.advertise_modifier_state(self);
        Some((original, keycode))
    }

    fn remote_restore_keymap(&mut self, original: String) {
        self.arbor.remote_input.temp_keymap_active = false;
        let keyboard = self.arbor.seat.get_keyboard().unwrap();
        let mods = keyboard.modifier_state();
        if let Err(err) = keyboard.set_keymap_from_string(self, original) {
            warn!("error restoring the keymap: {err:?}");
            return;
        }
        keyboard.set_modifier_state(mods);
        keyboard.advertise_modifier_state(self);
    }

    /// Resolves an absolute position on a stream target to an event on the output under it.
    fn remote_abs_event(
        &self,
        target: &AbsTarget,
        x: f64,
        y: f64,
        slot: u32,
        time: u64,
    ) -> Option<RemoteAbsEvent> {
        let global = self.remote_abs_global(target, x, y)?;
        let (output, local) = self
            .arbor
            .output_under(global)
            .map(|(output, local)| (output.clone(), local))?;
        let output_geo = self.arbor.global_space.output_geometry(&output)?;
        // Undo the transform that `compute_absolute_location` applies.
        let transform = output.current_transform();
        let pos = transform
            .invert()
            .transform_point_in(local, &output_geo.size.to_f64());
        Some(RemoteAbsEvent {
            device: RemoteDevice {
                output: Some(output),
            },
            pos,
            slot,
            time,
        })
    }

    /// Global logical position of (x, y) on `target`.
    fn remote_abs_global(&self, target: &AbsTarget, x: f64, y: f64) -> Option<Point<f64, Logical>> {
        let (origin, size): (Point<f64, Logical>, _) = match target {
            AbsTarget::Output(name) => {
                let output = self
                    .arbor
                    .global_space
                    .outputs()
                    .find(|o| o.name() == *name)?;
                let geo = self.arbor.global_space.output_geometry(output)?;
                (geo.loc.to_f64(), geo.size.to_f64())
            }
            AbsTarget::Window(id) => {
                // Only the active window of an output has a known on-screen rectangle; a remote
                // session driving a window stream works with that window focused.
                let rect = self.arbor.layout.monitors().find_map(|mon| {
                    let window = mon.active_window()?;
                    if window.id().get() != *id {
                        return None;
                    }
                    let rect: Rectangle<f64, Logical> = mon.active_tile_visual_rectangle()?;
                    let geo = self.arbor.global_space.output_geometry(mon.output())?;
                    Some(Rectangle::new(rect.loc + geo.loc.to_f64(), rect.size))
                })?;
                (rect.loc, rect.size)
            }
        };
        // Keep the point inside the target so that it never lands on a neighbour.
        let max_x = (size.w - 1.).max(0.);
        let max_y = (size.h - 1.).max(0.);
        Some(origin + Point::from((x.clamp(0., max_x), y.clamp(0., max_y))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(layout: &str, variant: &str, options: Option<String>) -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            layout,
            variant,
            options,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("keymap compiles (xkeyboard-config available)")
    }

    fn evdev(code: u32) -> Keycode {
        Keycode::new(code + EVDEV_OFFSET)
    }

    const KEY_A: u32 = 30;
    const KEY_1: u32 = 2;
    const KEY_E: u32 = 18;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_ENTER: u32 = 28;

    fn mod_mask(keymap: &xkb::Keymap, name: &str) -> xkb::ModMask {
        1 << keymap.mod_get_index(name)
    }

    #[test]
    fn plain_and_shifted_keysyms() {
        let km = keymap("us", "", None);
        assert_eq!(
            plan_keysym(&km, 0, 0, Keysym::a),
            Some(KeysymPlan {
                keycode: evdev(KEY_A),
                mods: vec![]
            })
        );
        assert_eq!(
            plan_keysym(&km, 0, 0, Keysym::A),
            Some(KeysymPlan {
                keycode: evdev(KEY_A),
                mods: vec![evdev(KEY_LEFTSHIFT)]
            })
        );
        assert_eq!(
            plan_keysym(&km, 0, 0, Keysym::exclam),
            Some(KeysymPlan {
                keycode: evdev(KEY_1),
                mods: vec![evdev(KEY_LEFTSHIFT)]
            })
        );
        assert_eq!(
            plan_keysym(&km, 0, 0, Keysym::Return),
            Some(KeysymPlan {
                keycode: evdev(KEY_ENTER),
                mods: vec![]
            })
        );
    }

    #[test]
    fn caps_lock_is_taken_into_account() {
        let km = keymap("us", "", None);
        let lock = mod_mask(&km, xkb::MOD_NAME_CAPS);
        // With Caps Lock on, the bare key gives "A" and Shift gives "a".
        assert_eq!(
            plan_keysym(&km, 0, lock, Keysym::A),
            Some(KeysymPlan {
                keycode: evdev(KEY_A),
                mods: vec![]
            })
        );
        assert_eq!(
            plan_keysym(&km, 0, lock, Keysym::a),
            Some(KeysymPlan {
                keycode: evdev(KEY_A),
                mods: vec![evdev(KEY_LEFTSHIFT)]
            })
        );
    }

    #[test]
    fn level3_keysyms_use_altgr() {
        let km = keymap("us", "intl", None);
        // us(intl): AltGr+e is eacute.
        let plan = plan_keysym(&km, 0, 0, Keysym::eacute).unwrap();
        assert_eq!(plan.keycode, evdev(KEY_E));
        // Some key that produces ISO_Level3_Shift (RightAlt or the <LVL3> key).
        assert_eq!(plan.mods.len(), 1);
        assert_eq!(
            km.key_get_syms_by_level(plan.mods[0], 0, 0),
            [Keysym::ISO_Level3_Shift]
        );
    }

    #[test]
    fn second_layout_is_used_when_active() {
        let km = keymap("us,ru", "", None);
        assert!(plan_keysym(&km, 0, 0, Keysym::Cyrillic_ef).is_none());
        let plan = plan_keysym(&km, 1, 0, Keysym::Cyrillic_ef).unwrap();
        assert_eq!(plan.keycode, evdev(KEY_A));
        assert!(plan.mods.is_empty());
    }

    #[test]
    fn unmapped_keysym_gets_a_spare_key() {
        let km = keymap("us", "", None);
        let em_dash = Keysym::emdash;
        assert!(plan_keysym(&km, 0, 0, em_dash).is_none());

        let spare = spare_keycode(&km).expect("us keymap has unused key codes");
        assert!(spare.raw() <= 255);
        let name = km.key_get_name(spare).unwrap().to_owned();
        let text = km.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        let augmented = keymap_with_extra_key(&text, &name, em_dash).unwrap();

        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let km2 = xkb::Keymap::new_from_string(
            &context,
            augmented,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("augmented keymap compiles");
        assert_eq!(
            plan_keysym(&km2, 0, 0, em_dash),
            Some(KeysymPlan {
                keycode: spare,
                mods: vec![]
            })
        );
        // Everything else is unchanged.
        assert_eq!(
            plan_keysym(&km2, 0, 0, Keysym::A),
            plan_keysym(&km, 0, 0, Keysym::A)
        );

        // Unicode keysyms (what clients send for arbitrary characters) work the same way.
        let snowman = Keysym::new(0x0100_2603);
        let augmented = keymap_with_extra_key(&text, &name, snowman).unwrap();
        let km3 = xkb::Keymap::new_from_string(
            &context,
            augmented,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let state = xkb::State::new(&km3);
        assert_eq!(state.key_get_one_sym(spare), snowman);
    }
}

use crate::appearance::{Color, WorkspaceShadow, WorkspaceShadowPart, DEFAULT_BACKDROP_COLOR};
use crate::utils::{Flag, FloatOrInt, MergeWith};

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct SpawnAtStartup {
    #[knuffel(arguments)]
    pub command: Vec<String>,
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct SpawnShAtStartup {
    #[knuffel(argument)]
    pub command: String,
}

#[derive(Debug, PartialEq)]
pub struct Cursor {
    pub xcursor_theme: String,
    pub xcursor_size: u8,
    pub hide_when_typing: bool,
    pub hide_after_inactive_ms: Option<u32>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            xcursor_theme: String::from("default"),
            xcursor_size: 24,
            hide_when_typing: false,
            hide_after_inactive_ms: None,
        }
    }
}

#[derive(knuffel::Decode, Debug, PartialEq)]
pub struct CursorPart {
    #[knuffel(child, unwrap(argument))]
    pub xcursor_theme: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub xcursor_size: Option<u8>,
    #[knuffel(child)]
    pub hide_when_typing: Option<Flag>,
    #[knuffel(child, unwrap(argument))]
    pub hide_after_inactive_ms: Option<u32>,
}

impl MergeWith<CursorPart> for Cursor {
    fn merge_with(&mut self, part: &CursorPart) {
        merge_clone!((self, part), xcursor_theme, xcursor_size);
        merge!((self, part), hide_when_typing);
        merge_clone_opt!((self, part), hide_after_inactive_ms);
    }
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq)]
pub struct ScreenshotPath(#[knuffel(argument)] pub Option<String>);

impl Default for ScreenshotPath {
    fn default() -> Self {
        Self(Some(String::from(
            "~/Pictures/Screenshots/Screenshot from %Y-%m-%d %H-%M-%S.png",
        )))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyOverlay {
    pub skip_at_startup: bool,
    pub hide_not_bound: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyOverlayPart {
    #[knuffel(child)]
    pub skip_at_startup: Option<Flag>,
    #[knuffel(child)]
    pub hide_not_bound: Option<Flag>,
}

impl MergeWith<HotkeyOverlayPart> for HotkeyOverlay {
    fn merge_with(&mut self, part: &HotkeyOverlayPart) {
        merge!((self, part), skip_at_startup, hide_not_bound);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfigNotification {
    pub disable_failed: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfigNotificationPart {
    #[knuffel(child)]
    pub disable_failed: Option<Flag>,
}

impl MergeWith<ConfigNotificationPart> for ConfigNotification {
    fn merge_with(&mut self, part: &ConfigNotificationPart) {
        merge!((self, part), disable_failed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clipboard {
    pub disable_primary: bool,
    /// Keep the clipboard (and primary selection) after the app that copied it exits.
    pub persist: bool,
    /// Cap on the data kept for `persist`, all formats of one selection together.
    pub max_size_mb: u32,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            disable_primary: false,
            persist: true,
            max_size_mb: 32,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPart {
    #[knuffel(child)]
    pub disable_primary: Option<Flag>,
    #[knuffel(child)]
    pub persist: Option<Flag>,
    #[knuffel(child, unwrap(argument))]
    pub max_size_mb: Option<u32>,
}

impl MergeWith<ClipboardPart> for Clipboard {
    fn merge_with(&mut self, part: &ClipboardPart) {
        merge!((self, part), disable_primary, persist);
        merge_clone!((self, part), max_size_mb);
    }
}

/// Plasma-style session restore: the running apps are saved on quit and relaunched at the next
/// start, each put back into its workspace and dock slot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionRestore {
    pub off: bool,
    /// App ids (exact) never saved or relaunched.
    pub exclude_app_ids: Vec<String>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionRestorePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(arguments))]
    pub exclude_app_ids: Option<Vec<String>>,
}

impl MergeWith<SessionRestorePart> for SessionRestore {
    fn merge_with(&mut self, part: &SessionRestorePart) {
        if part.off {
            self.off = true;
        }
        if part.on {
            self.off = false;
        }
        merge_clone!((self, part), exclude_app_ids);
    }
}

/// Accessibility: screen zoom (magnifier) and reduced motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Accessibility {
    /// Factor applied per `zoom-in` / `zoom-out` step.
    pub zoom_step: f64,
    /// Largest zoom level.
    pub zoom_max: f64,
    /// Turns animations off (like `animations { off; }`), for people sensitive to motion.
    pub reduce_motion: bool,
}

impl Default for Accessibility {
    fn default() -> Self {
        Self {
            zoom_step: 1.25,
            zoom_max: 16.,
            reduce_motion: false,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct AccessibilityPart {
    #[knuffel(child, unwrap(argument))]
    pub zoom_step: Option<FloatOrInt<1, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub zoom_max: Option<FloatOrInt<1, 100>>,
    #[knuffel(child)]
    pub reduce_motion: Option<Flag>,
}

impl MergeWith<AccessibilityPart> for Accessibility {
    fn merge_with(&mut self, part: &AccessibilityPart) {
        if let Some(step) = part.zoom_step {
            // A step of exactly 1 would never zoom.
            self.zoom_step = step.0.max(1.01);
        }
        if let Some(max) = part.zoom_max {
            self.zoom_max = max.0;
        }
        merge!((self, part), reduce_motion);
    }
}

/// Settings for arbor's own xdg-desktop-portal backend (`arbor.portal`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Portal {
    /// Wallpaper portal: command that sets the desktop wallpaper; the image path is appended.
    pub wallpaper_command: Option<Vec<String>>,
    /// Wallpaper portal: command that sets the lock screen wallpaper; the image path is
    /// appended.
    pub lockscreen_wallpaper_command: Option<Vec<String>>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct PortalPart {
    #[knuffel(child, unwrap(arguments))]
    pub wallpaper_command: Option<Vec<String>>,
    #[knuffel(child, unwrap(arguments))]
    pub lockscreen_wallpaper_command: Option<Vec<String>>,
}

impl MergeWith<PortalPart> for Portal {
    fn merge_with(&mut self, part: &PortalPart) {
        merge_clone_opt!(
            (self, part),
            wallpaper_command,
            lockscreen_wallpaper_command
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overview {
    pub backdrop_color: Color,
    pub workspace_shadow: WorkspaceShadow,
}

impl Default for Overview {
    fn default() -> Self {
        Self {
            backdrop_color: DEFAULT_BACKDROP_COLOR,
            workspace_shadow: WorkspaceShadow::default(),
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct OverviewPart {
    #[knuffel(child)]
    pub backdrop_color: Option<Color>,
    #[knuffel(child)]
    pub workspace_shadow: Option<WorkspaceShadowPart>,
}

impl MergeWith<OverviewPart> for Overview {
    fn merge_with(&mut self, part: &OverviewPart) {
        merge!((self, part), workspace_shadow);
        merge_clone!((self, part), backdrop_color);
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct Environment(#[knuffel(children)] pub Vec<EnvironmentVariable>);

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVariable {
    #[knuffel(node_name)]
    pub name: String,
    #[knuffel(argument)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XwaylandSatellite {
    pub off: bool,
    pub path: String,
}

impl Default for XwaylandSatellite {
    fn default() -> Self {
        Self {
            off: false,
            path: String::from("xwayland-satellite"),
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct XwaylandSatellitePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub path: Option<String>,
}

impl MergeWith<XwaylandSatellitePart> for XwaylandSatellite {
    fn merge_with(&mut self, part: &XwaylandSatellitePart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_clone!((self, part), path);
    }
}

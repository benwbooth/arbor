use std::ffi::OsString;
use std::path::PathBuf;

use arbor_ipc::{Action, OutputAction};
use clap::{Parser, Subcommand};
use clap_complete::Shell;

use crate::utils::version;

#[derive(Parser)]
#[command(name = "arbor", bin_name = "arbor", author, version = version(), long_about = None)]
#[command(about = "A Wayland compositor with a docking layout under a zoomable camera")]
#[command(args_conflicts_with_subcommands = true)]
#[command(subcommand_value_name = "SUBCOMMAND")]
#[command(subcommand_help_heading = "Subcommands")]
pub struct Cli {
    /// Path to config file (default: `$XDG_CONFIG_HOME/arbor/config.kdl`).
    ///
    /// This can also be set with the `ARBOR_CONFIG` environment variable. If both are set, the
    /// command line argument takes precedence.
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    /// Import environment globally to systemd and D-Bus, run D-Bus services.
    ///
    /// Set this flag in a systemd service started by your display manager, or when running
    /// manually as your main compositor instance. Do not set when running as a nested window, or
    /// on a TTY as your non-main compositor instance, to avoid messing up the global environment.
    #[arg(long)]
    pub session: bool,
    /// Command to run upon compositor startup.
    #[arg(last = true)]
    pub command: Vec<OsString>,

    #[command(subcommand)]
    pub subcommand: Option<Sub>,
}

#[derive(Subcommand)]
pub enum Sub {
    /// Communicate with the running arbor instance.
    Msg {
        #[command(subcommand)]
        msg: Msg,
        /// Format output as JSON.
        #[arg(short, long)]
        json: bool,
    },
    /// Validate the config file.
    Validate {
        /// Path to config file (default: `$XDG_CONFIG_HOME/arbor/config.kdl`).
        ///
        /// This can also be set with the `ARBOR_CONFIG` environment variable. If both are set, the
        /// command line argument takes precedence.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Cause a panic to check if the backtraces are good.
    Panic,
    /// Generate shell completions.
    Completions { shell: CompletionShell },
}

#[derive(Subcommand)]
pub enum Msg {
    /// List connected outputs.
    Outputs,
    /// List workspaces.
    Workspaces,
    /// List open windows.
    Windows,
    /// List open layer-shell surfaces.
    Layers,
    /// Get the configured keyboard layouts.
    KeyboardLayouts,
    /// Print information about the focused output.
    FocusedOutput,
    /// Print information about the focused window.
    FocusedWindow,
    /// Pick a window with the mouse and print information about it.
    PickWindow,
    /// Pick a color from the screen with the mouse.
    PickColor,
    /// Perform an action.
    Action {
        #[command(subcommand)]
        action: Action,
    },
    /// Change output configuration temporarily.
    ///
    /// The configuration is changed temporarily and not saved into the config file. If the output
    /// configuration subsequently changes in the config file, these temporary changes will be
    /// forgotten.
    Output {
        /// Output name.
        ///
        /// Run `arbor msg outputs` to see the output names.
        #[arg()]
        output: String,
        /// Configuration to apply.
        #[command(subcommand)]
        action: OutputAction,
    },
    /// Start continuously receiving events from the compositor.
    EventStream,
    /// Print the version of the running arbor instance.
    Version,
    /// Request an error from the running arbor instance.
    RequestError,
    /// Print the overview state.
    OverviewState,
    /// List screencasts.
    Casts,
    /// Set liquid glass shapes under a layer-shell surface (for testing; shells use IPC).
    ///
    /// Each --shape is X,Y,WIDTH,HEIGHT[,RADIUS] in the surface's logical coordinates. Without
    /// --shape the target's shapes are cleared.
    Glass {
        /// Layer-shell namespace of the surface (see `arbor msg layers`).
        #[arg(long)]
        namespace: String,
        /// Only the surface on this output.
        #[arg(long)]
        output: Option<String>,
        /// Target the surface's pop-ups instead.
        #[arg(long)]
        popup: bool,
        /// Key to keep several shape lists per namespace apart.
        #[arg(long, default_value = "")]
        key: String,
        /// A shape: X,Y,WIDTH,HEIGHT[,RADIUS]. Repeat for several.
        #[arg(long = "shape")]
        shapes: Vec<arbor_ipc::GlassShape>,
        /// Variant for every shape: regular or clear.
        #[arg(long)]
        variant: Option<arbor_ipc::GlassVariant>,
        /// Strength 0..1 for every shape.
        #[arg(long)]
        strength: Option<f64>,
        /// Blur radius (logical px) for every shape.
        #[arg(long)]
        blur_radius: Option<f64>,
    },
    /// List the global shortcuts apps registered (GlobalShortcuts portal and KDE kglobalaccel).
    GlobalShortcuts,
    /// Copy shortcut assignments from KDE Plasma's kglobalshortcutsrc into arbor's store.
    ///
    /// arbor does this by itself once, when its store doesn't exist yet. Plasma's own
    /// components (KWin, plasmashell, power, volume, media keys) are skipped.
    ImportKdeShortcuts {
        /// Replace assignments arbor already has.
        #[arg(long)]
        overwrite: bool,
    },
    /// Change or clear the trigger of an app's portal global shortcut.
    SetGlobalShortcut {
        /// App id of the shortcut (see `arbor msg global-shortcuts`; may be "").
        app_id: String,
        /// The app's shortcut id.
        id: String,
        /// New trigger, e.g. `Ctrl+Alt+R` or `Mod+Shift+T`.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        trigger: Option<String>,
        /// Remove the assignment instead.
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
    Nushell,
}

impl TryFrom<CompletionShell> for Shell {
    type Error = &'static str;

    fn try_from(shell: CompletionShell) -> Result<Self, Self::Error> {
        match shell {
            CompletionShell::Bash => Ok(Shell::Bash),
            CompletionShell::Elvish => Ok(Shell::Elvish),
            CompletionShell::Fish => Ok(Shell::Fish),
            CompletionShell::PowerShell => Ok(Shell::PowerShell),
            CompletionShell::Zsh => Ok(Shell::Zsh),
            CompletionShell::Nushell => Err("Nushell should be handled separately"),
        }
    }
}

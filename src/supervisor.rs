//! Restarting arbor in place: a new build, or after a crash, without logging out.
//!
//! Modelled on KWin's `kwin_wayland_wrapper`. `arbor --session` (or `arbor --supervise`) starts a
//! small supervisor process that owns the listening sockets that clients connect to:
//!
//! - the Wayland socket (`$WAYLAND_DISPLAY`),
//! - the IPC socket (`$ARBOR_SOCKET`),
//! - the X11 display sockets for xwayland-satellite (`$DISPLAY`),
//!
//! and runs the compositor as its child with those sockets inherited. When the compositor exits
//! for a reload (`arbor msg action reload-binary`) or crashes, the supervisor starts it again on
//! the same sockets. Connections made meanwhile wait in the socket backlog, so clients that
//! reconnect (Qt apps with `QT_WAYLAND_RECONNECT=1`, hearth's IPC connections) find the new
//! compositor under the same names. Before a reload the compositor saves its layout (see
//! `session.rs`), and the new one puts reconnecting windows back and relaunches apps that died.
//!
//! The child gets the sockets as file descriptors named in `ARBOR_SUPERVISED_*` environment
//! variables, plus a pipe for messages to the supervisor:
//!
//! - `READY`: started (the supervisor tells systemd once);
//! - `RELOAD <path>`: exiting with [`RELOAD_EXIT_CODE`] to run `<path>` next;
//! - `LOCKED` / `UNLOCKED`: the session lock state. A compositor that crashes while the screen is
//!   locked is not restarted (that would unlock it): the session ends instead, as without the
//!   supervisor.
//!
//! A build that crashes within [`PROBATION`] of a reload is replaced by the previous binary (kept
//! open by the supervisor, so a newer file at the same path doesn't matter). Crashes after that
//! restart the same binary, up to [`MAX_CRASHES`] within [`CRASH_WINDOW`].

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::{env, io, thread};

use anyhow::Context as _;
use smithay::reexports::rustix::io::{fcntl_getfd, fcntl_setfd, FdFlags};

/// Exit code of a compositor that wants to be replaced by the binary it named.
pub const RELOAD_EXIT_CODE: i32 = 75;
/// A reloaded build that crashes this soon is replaced by the previous one.
pub const PROBATION: Duration = Duration::from_secs(20);
/// More crashes than this within [`CRASH_WINDOW`] end the session.
pub const MAX_CRASHES: usize = 3;
pub const CRASH_WINDOW: Duration = Duration::from_secs(60);

const ENV_CONTROL: &str = "ARBOR_SUPERVISED_CONTROL_FD";
const ENV_WAYLAND_FD: &str = "ARBOR_SUPERVISED_WAYLAND_FD";
const ENV_WAYLAND_NAME: &str = "ARBOR_SUPERVISED_WAYLAND_NAME";
const ENV_IPC_FD: &str = "ARBOR_SUPERVISED_IPC_FD";
const ENV_IPC_PATH: &str = "ARBOR_SUPERVISED_IPC_PATH";
/// With the proxy: the channel IPC connections arrive on (wl_proxy/ipc.rs).
const ENV_IPC_CLIENTS_FD: &str = "ARBOR_SUPERVISED_IPC_CLIENTS_FD";
const ENV_X11_DISPLAY: &str = "ARBOR_SUPERVISED_X11_DISPLAY";
const ENV_X11_UNIX_FD: &str = "ARBOR_SUPERVISED_X11_UNIX_FD";
const ENV_X11_ABSTRACT_FD: &str = "ARBOR_SUPERVISED_X11_ABSTRACT_FD";
const ENV_CLIENTS_FD: &str = "ARBOR_SUPERVISED_CLIENTS_FD";
/// The supervisor runs xwayland-satellite for this display itself.
const ENV_X11_BY_SUPERVISOR: &str = "ARBOR_SUPERVISED_X11_BY_SUPERVISOR";
const ENV_START: &str = "ARBOR_SUPERVISED_START";
const ENV_STATE: &str = "ARBOR_SUPERVISED_STATE";
/// The previous compositor crashed while the screen was locked: start locked.
const ENV_START_LOCKED: &str = "ARBOR_SUPERVISED_START_LOCKED";

/// Why the compositor is starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    /// Not supervised, or the supervisor's first start: a fresh session.
    First,
    /// After `reload-binary`: the previous compositor saved its layout.
    Reload,
    /// After a crash: the layout from the last periodic save.
    Crash,
}

impl StartKind {
    fn as_str(self) -> &'static str {
        match self {
            StartKind::First => "first",
            StartKind::Reload => "reload",
            StartKind::Crash => "crash",
        }
    }

    pub fn is_restart(self) -> bool {
        self != StartKind::First
    }
}

/// What the supervisor handed this compositor process.
#[derive(Debug, Default)]
pub struct Inherited {
    pub wayland: Option<(UnixListener, OsString)>,
    /// With the supervisor's Wayland proxy (`wl_proxy`): the channel new connections arrive on,
    /// and the Wayland socket's name.
    pub proxy_clients: Option<(OwnedFd, OsString)>,
    /// The X11 display whose xwayland-satellite the supervisor runs (the compositor only
    /// exports `$DISPLAY`).
    pub x11_by_supervisor: Option<String>,
    pub ipc: Option<(UnixListener, PathBuf)>,
    /// With the proxy: the channel IPC connections arrive on, and the socket's path.
    pub ipc_proxy: Option<(OwnedFd, PathBuf)>,
    pub x11: Option<InheritedX11>,
}

#[derive(Debug)]
pub struct InheritedX11 {
    pub display_name: String,
    pub unix_fd: OwnedFd,
    pub abstract_fd: Option<OwnedFd>,
}

struct ChildSide {
    control: Option<File>,
    start: StartKind,
    state_path: Option<PathBuf>,
}

static INHERITED: Mutex<Option<Inherited>> = Mutex::new(None);
static CHILD: Mutex<Option<ChildSide>> = Mutex::new(None);

static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Whether this compositor started for an in-place restart less than `within` ago (clients are
/// reconnecting).
pub fn restarted_within(within: Duration) -> bool {
    start_kind().is_restart() && STARTED.get().is_some_and(|t| t.elapsed() < within)
}

/// Whether this process runs under the supervisor (call [`take_from_env`] first).
pub fn is_supervised() -> bool {
    CHILD.lock().unwrap().is_some()
}

/// Why this process started.
pub fn start_kind() -> StartKind {
    CHILD
        .lock()
        .unwrap()
        .as_ref()
        .map_or(StartKind::First, |c| c.start)
}

/// Where the layout is saved for the next compositor of this supervisor.
pub fn state_path() -> Option<PathBuf> {
    CHILD.lock().unwrap().as_ref()?.state_path.clone()
}

fn env_fd(name: &str) -> Option<OwnedFd> {
    let fd: RawFd = env::var(name).ok()?.parse().ok()?;
    env::remove_var(name);
    // SAFETY: the supervisor passed us this fd for our exclusive use.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // Not inherited by our own children.
    let _ = fcntl_setfd(&fd, FdFlags::CLOEXEC);
    Some(fd)
}

fn env_take(name: &str) -> Option<OsString> {
    let v = env::var_os(name);
    env::remove_var(name);
    v
}

/// Reads (and removes from the environment, so apps don't see them) what the supervisor passed.
/// Call once, early in `main`, before threads start.
pub fn take_from_env() {
    let Some(control) = env_fd(ENV_CONTROL) else {
        return;
    };
    let start = match env_take(ENV_START).as_deref().and_then(|s| s.to_str()) {
        Some("reload") => StartKind::Reload,
        Some("crash") => StartKind::Crash,
        _ => StartKind::First,
    };
    let state_path = env_take(ENV_STATE).map(PathBuf::from);
    if env_take(ENV_START_LOCKED).is_some() {
        START_LOCKED.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    let mut inherited = Inherited::default();
    let wayland_name = env_take(ENV_WAYLAND_NAME);
    if let (Some(fd), Some(name)) = (env_fd(ENV_CLIENTS_FD), wayland_name.clone()) {
        inherited.proxy_clients = Some((fd, name));
        PROXIED.store(true, std::sync::atomic::Ordering::Relaxed);
    } else if let (Some(fd), Some(name)) = (env_fd(ENV_WAYLAND_FD), wayland_name) {
        let listener = UnixListener::from(fd);
        let _ = listener.set_nonblocking(true);
        inherited.wayland = Some((listener, name));
    }
    let ipc_path = env_take(ENV_IPC_PATH);
    if let (Some(fd), Some(path)) = (env_fd(ENV_IPC_CLIENTS_FD), ipc_path.clone()) {
        inherited.ipc_proxy = Some((fd, PathBuf::from(path)));
    } else if let (Some(fd), Some(path)) = (env_fd(ENV_IPC_FD), ipc_path) {
        let listener = UnixListener::from(fd);
        let _ = listener.set_nonblocking(true);
        inherited.ipc = Some((listener, PathBuf::from(path)));
    }
    let x11_display = env_take(ENV_X11_DISPLAY);
    if env_take(ENV_X11_BY_SUPERVISOR).is_some() {
        inherited.x11_by_supervisor = x11_display
            .as_ref()
            .map(|d| d.to_string_lossy().into_owned());
    }
    let x11_abstract = env_fd(ENV_X11_ABSTRACT_FD);
    if let (Some(unix_fd), Some(display)) = (env_fd(ENV_X11_UNIX_FD), x11_display) {
        inherited.x11 = Some(InheritedX11 {
            display_name: display.to_string_lossy().into_owned(),
            unix_fd,
            abstract_fd: x11_abstract,
        });
    }

    let _ = STARTED.set(Instant::now());
    *INHERITED.lock().unwrap() = Some(inherited);
    *CHILD.lock().unwrap() = Some(ChildSide {
        control: Some(File::from(control)),
        start,
        state_path,
    });
}

/// The inherited Wayland listening socket and its name.
pub fn take_wayland() -> Option<(UnixListener, OsString)> {
    INHERITED.lock().unwrap().as_mut()?.wayland.take()
}

static PROXIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the supervisor proxies the Wayland socket (apps' connections outlive this process).
pub fn is_proxied() -> bool {
    PROXIED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The supervisor's clients channel (it proxies the Wayland socket) and the socket's name.
pub fn take_proxy_clients() -> Option<(OwnedFd, OsString)> {
    INHERITED.lock().unwrap().as_mut()?.proxy_clients.take()
}

/// The inherited IPC listening socket and its path.
/// With the proxy: the channel IPC connections arrive on, and the socket's path.
pub fn take_ipc_proxy() -> Option<(OwnedFd, PathBuf)> {
    INHERITED.lock().unwrap().as_mut()?.ipc_proxy.take()
}

pub fn take_ipc() -> Option<(UnixListener, PathBuf)> {
    INHERITED.lock().unwrap().as_mut()?.ipc.take()
}

/// The X11 display whose xwayland-satellite the supervisor runs, if it does.
pub fn x11_by_supervisor() -> Option<String> {
    INHERITED
        .lock()
        .unwrap()
        .as_ref()?
        .x11_by_supervisor
        .clone()
}

/// The inherited X11 display sockets.
pub fn take_x11() -> Option<InheritedX11> {
    INHERITED.lock().unwrap().as_mut()?.x11.take()
}

fn send(line: &str) {
    let mut child = CHILD.lock().unwrap();
    let Some(control) = child.as_mut().and_then(|c| c.control.as_mut()) else {
        return;
    };
    if let Err(err) = control.write_all(format!("{line}\n").as_bytes()) {
        warn!("error writing to the supervisor: {err:?}");
    }
}

/// Tells the supervisor this compositor is up.
pub fn notify_ready() {
    send("READY");
    // This build starts locked when asked to (a restart after a crash while locked).
    send("CAN_START_LOCKED");
}

static START_LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The previous compositor crashed while the screen was locked: this one starts locked (until
/// the lock screen locks again) instead of showing the session.
pub fn start_locked() -> bool {
    START_LOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Tells the supervisor the session lock state.
pub fn notify_locked(locked: bool) {
    send(if locked { "LOCKED" } else { "UNLOCKED" });
}

static RELOAD_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tells the supervisor to start `binary` (default: the binary it was started from, which may
/// have been replaced by a newer build since) after this process exits with
/// [`RELOAD_EXIT_CODE`].
pub fn request_reload(binary: Option<&Path>) {
    match binary {
        Some(binary) => send(&format!("RELOAD {}", binary.display())),
        None => send("RELOAD"),
    }
    RELOAD_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Whether this process should exit with [`RELOAD_EXIT_CODE`].
pub fn reload_requested() -> bool {
    RELOAD_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

// --- The supervisor side ---

/// Sockets the supervisor keeps open for all its compositors.
struct Sockets {
    // Keeps the lock file and unlinks the socket on drop.
    wayland: wayland_server::ListeningSocket,
    wayland_name: OsString,
    ipc: UnixListener,
    ipc_path: PathBuf,
    x11: Option<crate::utils::xwayland::X11Connection>,
}

impl Drop for Sockets {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.ipc_path);
    }
}

fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(env::temp_dir)
}

fn open_sockets(x11: bool) -> anyhow::Result<Sockets> {
    let wayland = wayland_server::ListeningSocket::bind_auto("wayland", 1..33)
        .context("error binding the Wayland socket")?;
    let wayland_name = wayland.socket_name().unwrap().to_os_string();

    let ipc_path = runtime_dir().join(format!(
        "arbor.{}.{}.sock",
        wayland_name.to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&ipc_path);
    let ipc = UnixListener::bind(&ipc_path).context("error binding the IPC socket")?;

    let x11 = if x11 {
        match crate::utils::xwayland::setup_connection() {
            Ok(x11) => Some(x11),
            Err(err) => {
                warn!("error opening X11 sockets: {err:?}");
                None
            }
        }
    } else {
        None
    };

    Ok(Sockets {
        wayland,
        wayland_name,
        ipc,
        ipc_path,
        x11,
    })
}

/// A binary to run, kept open so that replacing the file doesn't change what we run.
struct Binary {
    file: File,
    path: PathBuf,
    /// Said it can start locked (CAN_START_LOCKED) when it ran.
    can_start_locked: bool,
}

impl Binary {
    fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            path: path.to_owned(),
            can_start_locked: false,
        })
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            path: self.path.clone(),
            can_start_locked: self.can_start_locked,
        })
    }
}

enum Msg {
    Ready,
    CanStartLocked,
    Reload(PathBuf),
    Locked(bool),
}

fn clear_cloexec(fd: BorrowedFd) -> io::Result<()> {
    let flags = fcntl_getfd(fd)?;
    fcntl_setfd(fd, flags - FdFlags::CLOEXEC)?;
    Ok(())
}

struct Supervisor {
    sockets: Sockets,
    args: Vec<OsString>,
    state_path: PathBuf,
    /// Lines from the current child.
    msgs: std::sync::mpsc::Receiver<(u64, Msg)>,
    msgs_tx: std::sync::mpsc::Sender<(u64, Msg)>,
    generation: u64,
    notified_systemd: bool,
    /// The Wayland proxy, unless `ARBOR_NO_PROXY=1`.
    hub: Option<std::sync::Arc<crate::wl_proxy::Hub>>,
    /// The supervisor runs xwayland-satellite (with the proxy).
    x11_by_supervisor: bool,
    /// The current compositor said it can start locked.
    child_can_start_locked: bool,
    /// Start the next compositor locked (the previous one crashed while locked).
    start_locked: bool,
}

impl Supervisor {
    /// Starts a compositor; with the proxy, also returns the supervisor's end of its clients
    /// channel.
    fn spawn(
        &mut self,
        binary: &Binary,
        start: StartKind,
    ) -> io::Result<(Child, Option<(OwnedFd, OwnedFd)>)> {
        let (read, write) = smithay::reexports::rustix::pipe::pipe_with(
            smithay::reexports::rustix::pipe::PipeFlags::CLOEXEC,
        )?;
        self.generation += 1;
        let generation = self.generation;
        let tx = self.msgs_tx.clone();
        thread::Builder::new()
            .name("supervisor-pipe".to_owned())
            .spawn(move || {
                for line in BufReader::new(File::from(read)).lines() {
                    let Ok(line) = line else { break };
                    let msg = match line.split_once(' ') {
                        Some(("RELOAD", path)) => Msg::Reload(PathBuf::from(path)),
                        _ => match line.as_str() {
                            "READY" => Msg::Ready,
                            "CAN_START_LOCKED" => Msg::CanStartLocked,
                            "LOCKED" => Msg::Locked(true),
                            "UNLOCKED" => Msg::Locked(false),
                            _ => continue,
                        },
                    };
                    if tx.send((generation, msg)).is_err() {
                        break;
                    }
                }
            })?;

        let s = &self.sockets;
        let mut fds: Vec<RawFd> = vec![write.as_raw_fd()];
        let mut cmd = Command::new(format!("/proc/self/fd/{}", binary.file.as_raw_fd()));
        cmd.arg0(&binary.path)
            .args(&self.args)
            .env(ENV_CONTROL, write.as_raw_fd().to_string())
            .env(ENV_WAYLAND_NAME, &s.wayland_name)
            .env(ENV_IPC_PATH, &s.ipc_path)
            .env(ENV_START, start.as_str())
            .env(ENV_STATE, &self.state_path);
        if self.start_locked {
            cmd.env(ENV_START_LOCKED, "1");
        }
        let mut clients = None;
        if self.hub.is_some() {
            let (ours, theirs) = crate::wl_proxy::seqpacket_pair()?;
            cmd.env(ENV_CLIENTS_FD, theirs.as_raw_fd().to_string());
            fds.push(theirs.as_raw_fd());
            let (ipc_ours, ipc_theirs) = crate::wl_proxy::seqpacket_pair()?;
            cmd.env(ENV_IPC_CLIENTS_FD, ipc_theirs.as_raw_fd().to_string());
            fds.push(ipc_theirs.as_raw_fd());
            clients = Some(((ours, theirs), (ipc_ours, ipc_theirs)));
        } else {
            cmd.env(ENV_WAYLAND_FD, s.wayland.as_fd().as_raw_fd().to_string())
                .env(ENV_IPC_FD, s.ipc.as_raw_fd().to_string());
            fds.push(s.wayland.as_fd().as_raw_fd());
            fds.push(s.ipc.as_raw_fd());
        }
        if let (Some(x11), true) = (&s.x11, self.x11_by_supervisor) {
            cmd.env(ENV_X11_DISPLAY, &x11.display_name)
                .env(ENV_X11_BY_SUPERVISOR, "1");
        } else if let Some(x11) = &s.x11 {
            cmd.env(ENV_X11_DISPLAY, &x11.display_name)
                .env(ENV_X11_UNIX_FD, x11.unix_fd.as_raw_fd().to_string());
            fds.push(x11.unix_fd.as_raw_fd());
            if let Some(fd) = &x11.abstract_fd {
                cmd.env(ENV_X11_ABSTRACT_FD, fd.as_raw_fd().to_string());
                fds.push(fd.as_raw_fd());
            }
        }
        // SAFETY: only async-signal-safe calls (fcntl, sigprocmask) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                for fd in &fds {
                    clear_cloexec(BorrowedFd::borrow_raw(*fd))?;
                }
                crate::utils::signals::unblock_all()
            });
        }
        info!(
            "supervisor: starting {} ({})",
            binary.path.display(),
            start.as_str()
        );
        let child = cmd.spawn();
        drop(write);
        Ok((
            child?,
            clients.map(|((ours, _theirs), (ipc_ours, _ipc_theirs))| (ours, ipc_ours)),
        ))
    }

    fn drain(&mut self, generation: u64, locked: &mut bool, reload: &mut Option<PathBuf>) {
        while let Ok((gen, msg)) = self.msgs.try_recv() {
            if gen != generation {
                continue;
            }
            match msg {
                Msg::Ready => {
                    if !self.notified_systemd {
                        self.notified_systemd = true;
                        if let Err(err) = sd_notify::notify(&[sd_notify::NotifyState::Ready]) {
                            warn!("error notifying systemd: {err:?}");
                        }
                    }
                }
                Msg::Locked(l) => *locked = l,
                Msg::CanStartLocked => self.child_can_start_locked = true,
                Msg::Reload(path) => *reload = Some(path),
            }
        }
    }
}

/// Signals the supervisor forwards to the compositor (and then exits with it).
static TERMINATING: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static CHILD_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn forward_signal(sig: libc::c_int) {
    TERMINATING.store(sig, std::sync::atomic::Ordering::SeqCst);
    let pid = CHILD_PID.load(std::sync::atomic::Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
}

/// Runs the supervisor: starts the compositor (this binary, with `args`) and restarts it on
/// reloads and crashes. Returns the exit code for the process.
/// `satellite`: xwayland-satellite's path, or None without X11.
pub fn run(args: Vec<OsString>, satellite: Option<String>) -> anyhow::Result<i32> {
    let exe = env::current_exe().context("error finding our own binary")?;
    let mut current = Binary::open(&exe).context("error opening our own binary")?;
    let sockets = open_sockets(satellite.is_some())?;
    info!(
        "supervisor: Wayland socket {}, IPC {}",
        sockets.wayland_name.to_string_lossy(),
        sockets.ipc_path.display()
    );
    let state_path = runtime_dir().join(format!("arbor-restart.{}.json", std::process::id()));

    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only does atomics and kill().
        unsafe {
            libc::signal(
                sig,
                forward_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
    }

    let (msgs_tx, msgs) = std::sync::mpsc::channel();
    let mut sup = Supervisor {
        sockets,
        args,
        state_path: state_path.clone(),
        msgs,
        msgs_tx,
        generation: 0,
        notified_systemd: false,
        hub: None,
        x11_by_supervisor: false,
        child_can_start_locked: false,
        start_locked: false,
    };

    if env::var_os("ARBOR_NO_PROXY").is_some_and(|v| !v.is_empty() && v != "0") {
        info!("supervisor: ARBOR_NO_PROXY set; the compositor accepts Wayland clients itself");
    } else {
        let hub = std::sync::Arc::new(crate::wl_proxy::Hub::default());
        let started = sup
            .sockets
            .wayland
            .as_fd()
            .try_clone_to_owned()
            .map(UnixListener::from)
            .and_then(|l| crate::wl_proxy::start(l, hub.clone()))
            .and_then(|()| {
                let ipc = sup.sockets.ipc.try_clone()?;
                crate::wl_proxy::ipc::start(ipc, hub.clone())
            });
        match started {
            Ok(()) => sup.hub = Some(hub),
            Err(err) => warn!("supervisor: error starting the Wayland proxy: {err:?}"),
        }
    }
    // With the proxy, xwayland-satellite (and with it every X11 app) can outlive compositors.
    if let (Some(_), Some(x11), Some(path)) = (&sup.hub, &sup.sockets.x11, &satellite) {
        sup.x11_by_supervisor = crate::utils::xwayland::satellite::run_in_supervisor(
            path.clone(),
            x11,
            sup.sockets.wayland_name.clone(),
        );
    }

    let mut start = StartKind::First;
    // The binary to go back to if this one fails right after a reload.
    let mut previous: Option<Binary> = None;
    let mut crashes: Vec<Instant> = Vec::new();

    let code = loop {
        let started = Instant::now();
        let mut child = match sup.spawn(&current, start) {
            Ok((child, clients)) => {
                if let Some(hub) = &sup.hub {
                    hub.set(clients);
                }
                child
            }
            Err(err) => {
                warn!(
                    "supervisor: error starting {}: {err:?}",
                    current.path.display()
                );
                match previous.take() {
                    Some(prev) => {
                        current = prev;
                        start = StartKind::Crash;
                        continue;
                    }
                    None => break 1,
                }
            }
        };
        CHILD_PID.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
        // A signal that came before the pid was stored.
        let pending = TERMINATING.load(std::sync::atomic::Ordering::SeqCst);
        if pending != 0 {
            // SAFETY: plain kill() of our child.
            unsafe { libc::kill(child.id() as i32, pending) };
        }

        let generation = sup.generation;
        sup.child_can_start_locked = false;
        let mut locked = sup.start_locked;
        let mut reload = None;
        let status = loop {
            sup.drain(generation, &mut locked, &mut reload);
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => (),
                Err(err) => {
                    warn!("supervisor: error waiting for the compositor: {err:?}");
                    break std::process::ExitStatus::from_raw(1 << 8);
                }
            }
        };
        CHILD_PID.store(0, std::sync::atomic::Ordering::SeqCst);
        if let Some(hub) = &sup.hub {
            hub.set(None);
        }
        // The pipe's reader thread gets EOF with the child gone; give it a moment to deliver.
        thread::sleep(Duration::from_millis(20));
        sup.drain(generation, &mut locked, &mut reload);

        let term = TERMINATING.load(std::sync::atomic::Ordering::SeqCst);
        if term != 0 {
            info!("supervisor: compositor ended after signal {term}: {status}");
            break status.code().unwrap_or(0);
        }

        if sup.child_can_start_locked {
            current.can_start_locked = true;
        }
        match status.code() {
            Some(0) => {
                info!("supervisor: compositor quit");
                break 0;
            }
            Some(RELOAD_EXIT_CODE) => {
                let path = reload.unwrap_or_else(|| current.path.clone());
                match Binary::open(&path) {
                    Ok(next) => {
                        info!("supervisor: reloading into {}", path.display());
                        previous = Some(current);
                        current = next;
                    }
                    Err(err) => {
                        warn!(
                            "supervisor: error opening {}: {err:?}; restarting the same binary",
                            path.display()
                        );
                    }
                }
                start = StartKind::Reload;
                sup.start_locked = false;
                crashes.clear();
                continue;
            }
            _ => (),
        }

        // A crash.
        warn!("supervisor: compositor crashed: {status}");
        if sup.child_can_start_locked {
            current.can_start_locked = true;
        }
        sup.start_locked = false;
        if locked {
            // The next compositor starts locked (its outputs show the locked frame until the lock
            // screen locks again), so restarting doesn't unlock. Only a build known to do that.
            let next_ok = if started.elapsed() < PROBATION {
                previous
                    .as_ref()
                    .map_or(current.can_start_locked, |p| p.can_start_locked)
            } else {
                current.can_start_locked
            };
            if !next_ok {
                warn!(
                    "supervisor: the screen was locked and the next build can't start locked; \
                     ending the session instead of restarting"
                );
                break status.code().unwrap_or(1);
            }
            warn!("supervisor: the screen was locked; restarting locked");
            sup.start_locked = true;
        }
        if started.elapsed() < PROBATION {
            if let Some(prev) = previous.take() {
                warn!(
                    "supervisor: the new build failed; going back to {}",
                    prev.path.display()
                );
                current = prev;
                start = StartKind::Crash;
                continue;
            }
        }
        let now = Instant::now();
        crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        crashes.push(now);
        if crashes.len() > MAX_CRASHES {
            warn!("supervisor: too many crashes; ending the session");
            break status.code().unwrap_or(1);
        }
        // Later reloads fall back to this binary, which ran fine for a while.
        if started.elapsed() >= PROBATION {
            previous = None;
        }
        current = match current.try_clone() {
            Ok(b) => b,
            Err(_) => break 1,
        };
        start = StartKind::Crash;
    };

    let _ = std::fs::remove_file(&state_path);
    for ext in ["clipboard", "primary"] {
        let _ = std::fs::remove_file(state_path.with_extension(ext));
    }
    Ok(code)
}

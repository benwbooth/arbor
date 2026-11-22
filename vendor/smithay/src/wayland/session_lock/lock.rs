//! ext-session-lock lock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::wayland::compositor::SurfaceAttributes;
use crate::wayland::compositor::{self, BufferAssignment};
use _session_lock::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1;
use _session_lock::ext_session_lock_v1::{Error, ExtSessionLockV1, Request};
use wayland_protocols::ext::session_lock::v1::server::{self as _session_lock};
use wayland_server::protocol::wl_output::WlOutput;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

use crate::wayland::session_lock::surface::{ExtLockSurfaceUserData, LockSurface, LockSurfaceAttributes};
use crate::wayland::session_lock::{SessionLockHandler, SessionLockManagerState};

/// Surface role for ext-session-lock surfaces.
const LOCK_SURFACE_ROLE: &str = "ext_session_lock_surface_v1";

/// [`ExtSessionLockV1`] state.
#[derive(Debug)]
pub struct SessionLockState {
    pub(crate) lock_status: Arc<AtomicBool>,
    // arbor: the outputs this lock object has a surface for. Upstream keeps
    // one list in SessionLockManagerState for every lock object, cleared
    // only on unlock_and_destroy, so a client whose lock was refused (or
    // destroyed) could never lock that output again: its next lock's
    // get_lock_surface posted duplicated_output and killed the client.
    pub(crate) locked_outputs: Mutex<Vec<WlOutput>>,
}

impl SessionLockState {
    pub(crate) fn new() -> Self {
        Self {
            lock_status: Arc::new(AtomicBool::new(false)),
            locked_outputs: Mutex::new(Vec::new()),
        }
    }
}

impl<D> Dispatch<ExtSessionLockV1, SessionLockState, D> for SessionLockManagerState
where
    D: Dispatch<ExtSessionLockV1, SessionLockState>,
    D: Dispatch<ExtSessionLockSurfaceV1, ExtLockSurfaceUserData>,
    D: SessionLockHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        lock: &ExtSessionLockV1,
        request: Request,
        data: &SessionLockState,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            Request::GetLockSurface { id, surface, output } => {
                // Assign surface a role and ensure it never had one before.
                if compositor::give_role(&surface, LOCK_SURFACE_ROLE).is_err() {
                    lock.post_error(Error::Role, "Surface already has a role.");
                    return;
                }

                // Ensure output is not already locked (by this lock object; arbor).
                {
                    let mut locked_outputs = data.locked_outputs.lock().unwrap();
                    if locked_outputs.contains(&output) {
                        lock.post_error(Error::DuplicateOutput, "Output is already locked.");
                        return;
                    }
                    locked_outputs.push(output.clone());
                }

                // Ensure surface has no existing buffers attached.
                let has_buffer = compositor::with_states(&surface, |states| {
                    let cached = &states.cached_state;
                    let mut guard = cached.get::<SurfaceAttributes>();
                    let pending = matches!(guard.pending().buffer, Some(BufferAssignment::NewBuffer(_)));
                    let current = matches!(guard.current().buffer, Some(BufferAssignment::NewBuffer(_)));
                    pending || current
                });
                if has_buffer {
                    lock.post_error(Error::AlreadyConstructed, "Surface has a buffer attached.");
                    return;
                }

                let data = ExtLockSurfaceUserData {
                    surface: surface.downgrade(),
                };
                let lock_surface = data_init.init(id, data);

                // Initialize surface data.
                compositor::with_states(&surface, |states| {
                    let inserted = states.data_map.insert_if_missing_threadsafe(|| {
                        Mutex::new(LockSurfaceAttributes::new(lock_surface.clone()))
                    });

                    if !inserted {
                        let mut attributes = states
                            .data_map
                            .get::<Mutex<LockSurfaceAttributes>>()
                            .unwrap()
                            .lock()
                            .unwrap();
                        attributes.surface = lock_surface.clone();
                    }
                });

                // Add pre-commit hook for updating surface state.
                compositor::add_pre_commit_hook::<D, _>(&surface, LockSurface::pre_commit_hook);

                // Call compositor handler.
                let lock_surface = LockSurface::new(surface, lock_surface);
                state.new_surface(lock_surface.clone(), output);

                // Send initial configure when the interface is bound.
                lock_surface.send_configure();
            }
            Request::UnlockAndDestroy => {
                // Ensure session is locked.
                if !data.lock_status.load(Ordering::Relaxed) {
                    lock.post_error(Error::InvalidUnlock, "Session is not locked.");
                    // arbor: and stop there. Upstream went on to unlock, so a
                    // client whose lock was refused (the session already
                    // locked by another client) could unlock that session.
                    return;
                }

                data.locked_outputs.lock().unwrap().clear();
                state.unlock();
            }
            Request::Destroy => {
                // Ensure session is not locked.
                if data.lock_status.load(Ordering::Relaxed) {
                    lock.post_error(Error::InvalidDestroy, "Cannot destroy session lock while locked.");
                }
            }
            _ => unreachable!(),
        }
    }
}

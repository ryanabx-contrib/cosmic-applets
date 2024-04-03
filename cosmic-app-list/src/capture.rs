use cctk::{
    cosmic_protocols::{
        screencopy::v2::client::zcosmic_screencopy_session_v2,
        toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
    },
    screencopy::ScreencopyState,
    wayland_client::{Proxy, QueueHandle},
};
use cosmic::cctk;

use std::sync::{Arc, Mutex};

use crate::screencopy::{ScreencopySession, SessionData};
use crate::wayland_handler::AppData;

#[derive(Clone, Debug, Default)]
pub struct CaptureFilter {
    pub toplevels: Vec<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
}

pub struct Capture {
    pub source: zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    pub session: Mutex<Option<ScreencopySession>>,
}

impl Capture {
    pub fn new(source: zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1) -> Arc<Capture> {
        Arc::new(Capture {
            source,
            session: Mutex::new(None),
        })
    }

    // Returns `None` if capture is destroyed
    // (or if `session` wasn't created with `SessionData`)
    pub fn for_session(
        session: &zcosmic_screencopy_session_v2::ZcosmicScreencopySessionV2,
    ) -> Option<Arc<Self>> {
        session.data::<SessionData>()?.capture.upgrade()
    }

    // Start capturing frames
    pub fn start(self: &Arc<Self>, screencopy_state: &ScreencopyState, qh: &QueueHandle<AppData>) {
        let mut session = self.session.lock().unwrap();
        if session.is_none() {
            *session = Some(ScreencopySession::new(self, screencopy_state, qh));
        }
    }

    // Stop capturing. Can be started again with `start`
    pub fn stop(&self) {
        self.session.lock().unwrap().take();
    }
}

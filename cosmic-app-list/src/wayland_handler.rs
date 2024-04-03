use crate::capture::{Capture, CaptureFilter};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    os::{
        fd::{AsFd, FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
};

use calloop;
use calloop_wayland_source::WaylandSource;

use cctk::{
    screencopy::{
        capture, Formats, Frame, ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler,
        ScreencopySessionData, ScreencopySessionDataExt, ScreencopyState,
    },
    sctk::{
        self,
        activation::{RequestData, RequestDataExt},
        dmabuf::{DmabufFeedback, DmabufState},
        output::{OutputHandler, OutputState},
        seat::{SeatHandler, SeatState},
        shm::{Shm, ShmHandler},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        globals::registry_queue_init,
        protocol::{
            wl_buffer, wl_output,
            wl_seat::WlSeat,
            wl_shm::{self, WlShm},
            wl_shm_pool,
            wl_surface::WlSurface,
        },
        Connection, Dispatch, Proxy, QueueHandle, WEnum,
    },
    workspace::{WorkspaceHandler, WorkspaceState},
};
use cosmic::{iced, iced_sctk::subsurface_widget::SubsurfaceBuffer};
use cosmic_protocols::{
    image_source::v1::client::zcosmic_toplevel_image_source_manager_v1::ZcosmicToplevelImageSourceManagerV1,
    screencopy::v2::client::{
        zcosmic_screencopy_frame_v2, zcosmic_screencopy_manager_v2, zcosmic_screencopy_session_v2,
    },
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1::{
        self, State as ToplevelUpdateState, ZcosmicToplevelHandleV1,
    },
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    workspace::v1::client::zcosmic_workspace_handle_v1::{
        State as WorkspaceUpdateState, ZcosmicWorkspaceHandleV1,
    },
};
use futures::{channel::mpsc::UnboundedSender, executor::block_on, FutureExt, SinkExt};
use futures_channel::mpsc::{self, Sender};
use sctk::{
    activation::{ActivationHandler, ActivationState},
    registry::{ProvidesRegistryState, RegistryState},
};

pub struct AppData {
    pub exit: bool,
    pub sender: Sender<WaylandUpdate>,
    pub qh: QueueHandle<Self>,
    pub dmabuf_state: DmabufState,
    pub workspace_state: WorkspaceState,
    pub toplevel_info_state: ToplevelInfoState,
    pub toplevel_manager_state: ToplevelManagerState,
    pub screencopy_state: ScreencopyState,
    pub registry_state: RegistryState,
    pub seat_state: SeatState,
    pub shm_state: Shm,
    pub activation_state: Option<ActivationState>,
    pub output_state: OutputState,

    pub capture_filter: CaptureFilter,
    pub captures:
        RefCell<HashMap<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, Arc<Capture>>>,
    pub dmabuf_feedback: Option<DmabufFeedback>,
    pub gbm: Option<(PathBuf, gbm::Device<fs::File>)>,
    pub scheduler: calloop::futures::Scheduler<()>,
}

// Workspace and toplevel handling

// Need to bind output globals just so workspace can get output events
impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
    }
}

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        'workspaces_loop: for group in self.workspace_state.workspace_groups() {
            for workspace in &group.workspaces {
                if workspace
                    .state
                    .contains(&WEnum::Value(WorkspaceUpdateState::Active))
                {
                    self.send_event(WaylandUpdate::Workspace(WorkspaceUpdate::Enter(
                        workspace.handle.clone(),
                    )));
                    break 'workspaces_loop;
                }
            }
        }
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!();
}

pub struct ExecRequestData {
    data: RequestData,
    exec: String,
    gpu_idx: Option<usize>,
}

impl RequestDataExt for ExecRequestData {
    fn app_id(&self) -> Option<&str> {
        self.data.app_id()
    }

    fn seat_and_serial(&self) -> Option<(&WlSeat, u32)> {
        self.data.seat_and_serial()
    }

    fn surface(&self) -> Option<&WlSurface> {
        self.data.surface()
    }
}

impl ActivationHandler for AppData {
    type RequestData = ExecRequestData;
    fn new_token(&mut self, token: String, data: &ExecRequestData) {
        self.send_event(WaylandUpdate::ActivationToken {
            token: Some(token),
            exec: data.exec.clone(),
            gpu_idx: data.gpu_idx,
        });
    }
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut sctk::seat::SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut cctk::toplevel_management::ToplevelManagerState {
        &mut self.toplevel_manager_state
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
        // TODO capabilities could affect the options in the applet
    }
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            self.send_event(WaylandUpdate::Toplevel(ToplevelUpdate::Add(
                toplevel.clone(),
                info.clone(),
            )));
        }
        self.add_capture_source(toplevel.clone());
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            self.send_event(WaylandUpdate::Toplevel(ToplevelUpdate::Update(
                toplevel.clone(),
                info.clone(),
            )));
        }
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    ) {
        self.send_event(WaylandUpdate::Toplevel(ToplevelUpdate::Remove(
            toplevel.clone(),
        )));
        self.remove_capture_source(toplevel.clone());
    }
}

impl AppData {
    pub fn send_event(&mut self, update: WaylandUpdate) {
        let _ = block_on(self.sender.send(update));
    }

    fn matches_capture_filter(&self, source: &ZcosmicToplevelHandleV1) -> bool {
        self.capture_filter.toplevels.contains(source)
    }

    fn invalidate_capture_filter(&self) {
        for (source, capture) in self.captures.borrow_mut().iter_mut() {
            let matches = self.matches_capture_filter(source);
            if matches {
                capture.start(&self.screencopy_state, &self.qh);
            } else {
                capture.stop();
            }
        }
    }

    fn add_capture_source(&self, source: ZcosmicToplevelHandleV1) {
        self.captures
            .borrow_mut()
            .entry(source.clone())
            .or_insert_with(|| {
                let matches = self.matches_capture_filter(&source);
                let capture = Capture::new(source);
                if matches {
                    capture.start(&self.screencopy_state, &self.qh);
                }
                capture
            });
    }

    fn remove_capture_source(&self, source: ZcosmicToplevelHandleV1) {
        if let Some(capture) = self.captures.borrow_mut().remove(&source) {
            capture.stop();
        }
    }
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

#[derive(Clone, Debug)]
pub enum WaylandUpdate {
    Init(calloop::channel::Sender<WaylandRequest>),
    Finished,
    Toplevel(ToplevelUpdate),
    Workspace(WorkspaceUpdate),
    ActivationToken {
        token: Option<String>,
        exec: String,
        gpu_idx: Option<usize>,
    },
    ToplevelCapture(ZcosmicToplevelHandleV1, CaptureImage),
}

#[derive(Clone, Debug)]
pub enum ToplevelUpdate {
    Add(ZcosmicToplevelHandleV1, ToplevelInfo),
    Update(ZcosmicToplevelHandleV1, ToplevelInfo),
    Remove(ZcosmicToplevelHandleV1),
}

#[derive(Clone, Debug)]
pub enum WorkspaceUpdate {
    Enter(ZcosmicWorkspaceHandleV1),
}

#[derive(Clone, Debug)]
pub enum WaylandRequest {
    Toplevel(ToplevelRequest),
    TokenRequest {
        app_id: String,
        exec: String,
        gpu_idx: Option<usize>,
    },
    CaptureFilter(CaptureFilter),
}

#[derive(Debug, Clone)]
pub enum ToplevelRequest {
    Activate(ZcosmicToplevelHandleV1),
    Minimize(ZcosmicToplevelHandleV1),
    Quit(ZcosmicToplevelHandleV1),
}

pub fn subscription() -> iced::Subscription<WaylandUpdate> {
    iced::subscription::run_with_id("wayland-sub", async { start() }.flatten_stream())
}

fn start() -> mpsc::Receiver<WaylandUpdate> {
    let socket = std::env::var("X_PRIVILEGED_WAYLAND_SOCKET")
        .ok()
        .and_then(|fd| {
            fd.parse::<RawFd>()
                .ok()
                .map(|fd| unsafe { UnixStream::from_raw_fd(fd) })
        });

    let conn = if let Some(socket) = socket {
        Connection::from_socket(socket).unwrap()
    } else {
        Connection::connect_to_env().unwrap()
    };

    let (sender, receiver) = mpsc::channel(20);

    let (globals, event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let dmabuf_state = DmabufState::new(&globals, &qh);
    dmabuf_state.get_default_feedback(&qh).unwrap();

    thread::spawn(move || {
        let (executor, scheduler) = calloop::futures::executor().unwrap();

        let registry_state = RegistryState::new(&globals);
        let mut app_data = AppData {
            exit: false,
            sender,
            qh: qh.clone(),
            dmabuf_state,
            workspace_state: WorkspaceState::new(&registry_state, &qh),
            toplevel_info_state: ToplevelInfoState::new(&registry_state, &qh),
            toplevel_manager_state: ToplevelManagerState::new(&registry_state, &qh),
            screencopy_state: ScreencopyState::new(&globals, &qh),
            registry_state,
            seat_state: SeatState::new(&globals, &qh),
            shm_state: Shm::bind(&globals, &qh).unwrap(),
            activation_state: ActivationState::bind::<AppData>(&globals, &qh).ok(),
            output_state: OutputState::new(&globals, &qh),
            capture_filter: CaptureFilter::default(),
            captures: RefCell::new(HashMap::new()),
            dmabuf_feedback: None,
            gbm: None,
            scheduler,
        };

        // app_data.send_event(Event::Seats(app_data.seat_state.seats().collect()));
        // app_data.send_event(Event::ToplevelManager(
        //     app_data.toplevel_manager_state.manager.clone(),
        // ));
        // if let Ok(manager) = app_data.workspace_state.workspace_manager().get() {
        //     app_data.send_event(Event::WorkspaceManager(manager.clone()));
        // }

        let (cmd_sender, cmd_channel) = calloop::channel::channel();
        app_data.send_event(WaylandUpdate::Init(cmd_sender));

        let mut event_loop = calloop::EventLoop::try_new().unwrap();
        WaylandSource::new(conn, event_queue)
            .insert(event_loop.handle())
            .unwrap();
        event_loop
            .handle()
            .insert_source(cmd_channel, |event, _, state| match event {
                calloop::channel::Event::Msg(req) => match req {
                    WaylandRequest::CaptureFilter(filter) => {
                        state.capture_filter = filter;
                        println!(
                            "capturing '{}' toplevels",
                            state.capture_filter.toplevels.len()
                        );
                        state.invalidate_capture_filter();
                    }
                    WaylandRequest::Toplevel(req) => match req {
                        ToplevelRequest::Activate(handle) => {
                            if let Some(seat) = state.seat_state.seats().next() {
                                let manager = &state.toplevel_manager_state.manager;
                                manager.activate(&handle, &seat);
                            }
                        }
                        ToplevelRequest::Minimize(handle) => {
                            let manager = &state.toplevel_manager_state.manager;
                            manager.set_minimized(&handle);
                        }
                        ToplevelRequest::Quit(handle) => {
                            let manager = &state.toplevel_manager_state.manager;
                            manager.close(&handle);
                        }
                    },
                    WaylandRequest::TokenRequest {
                        app_id,
                        exec,
                        gpu_idx,
                    } => {
                        if let Some(activation_state) = state.activation_state.as_ref() {
                            activation_state.request_token_with_data(
                                &state.qh,
                                ExecRequestData {
                                    data: RequestData {
                                        app_id: Some(app_id),
                                        seat_and_serial: state
                                            .seat_state
                                            .seats()
                                            .next()
                                            .map(|seat| (seat, 0)),
                                        surface: None,
                                    },
                                    exec,
                                    gpu_idx,
                                },
                            );
                        } else {
                            state.send_event(WaylandUpdate::ActivationToken {
                                token: None,
                                exec,
                                gpu_idx,
                            });
                        }
                    }
                },
                calloop::channel::Event::Closed => {
                    state.exit = true;
                }
            })
            .unwrap();
        event_loop
            .handle()
            .insert_source(executor, |(), _, _| {})
            .unwrap();

        loop {
            if event_loop.dispatch(None, &mut app_data).is_err() {
                eprintln!("WTF");
            }
        }
    });

    receiver
}

// NEW SCREENCOPY STUFF

#[derive(Clone, Debug)]
pub struct CaptureImage {
    pub width: u32,
    pub height: u32,
    pub wl_buffer: SubsurfaceBuffer,
}

// END NEW SCREENCOPY STUFF

sctk::delegate_seat!(AppData);
sctk::delegate_registry!(AppData);
sctk::delegate_shm!(AppData);
cctk::delegate_toplevel_info!(AppData);
cctk::delegate_workspace!(AppData);
cctk::delegate_toplevel_manager!(AppData);

sctk::delegate_activation!(AppData, ExecRequestData);

sctk::delegate_output!(AppData);

//! Wayland client: tracks workspaces and windows via cctk and activates windows on request.
//!
//! Safety rule (cosmic-comp 2f7c34f, toplevel_management.rs:189-217 unwraps the window
//! behind every handle): the only request this program ever sends to the toplevel manager
//! is `activate`, and only on a handle that cctk currently lists in `toplevels()`. cctk only
//! lists a toplevel after it received cosmic info for it and removes it on `closed`, so we
//! never touch an empty or stale handle. The handle is looked up again right before sending.

use crate::model::{self, Candidate, Direction, Switcher};
use anyhow::{Context, Result, anyhow};
use cctk::{
    cosmic_protocols::{
        toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State as ToplevelState,
        toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    },
    sctk::{
        self,
        output::{OutputHandler, OutputState},
        reexports::{
            calloop,
            calloop_wayland_source::WaylandSource,
        },
        registry::{ProvidesRegistryState, RegistryState},
        seat::{Capability, SeatHandler, SeatState},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        Connection, QueueHandle, WEnum,
        backend::ObjectId,
        globals::registry_queue_init,
        protocol::{wl_output::WlOutput, wl_seat::WlSeat},
        Proxy,
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    },
    workspace::{WorkspaceHandler, WorkspaceState},
};
use serde::Serialize;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read-only: binds no toplevel manager, sends no requests.
    Watch,
    Daemon,
}

struct App {
    mode: Mode,
    conn: Connection,
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    workspace_state: WorkspaceState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: Option<ToplevelManagerState>,
    switcher: Switcher<String, ObjectId>,
    dirty: bool,
    last_printed: String,
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Activate,
    Close,
}

/// One entry of the `list` reply consumed by the launcher plugin.
#[derive(Serialize, serde::Deserialize, Debug)]
pub struct ListedWindow {
    pub id: u32,
    pub app_id: String,
    pub title: String,
}

#[derive(Serialize)]
struct WindowView {
    app_id: String,
    title: String,
    focused: bool,
    minimized: bool,
}

#[derive(Serialize)]
struct WorkspaceView {
    name: String,
    outputs: Vec<String>,
    active: bool,
    current: bool,
    windows: Vec<WindowView>,
}

#[derive(Serialize)]
struct Snapshot {
    workspaces: Vec<WorkspaceView>,
    /// Windows the compositor reports on no known workspace.
    other_windows: Vec<WindowView>,
}

fn window_view(t: &ToplevelInfo) -> WindowView {
    WindowView {
        app_id: t.app_id.clone(),
        title: t.title.clone(),
        focused: t.state.contains(&ToplevelState::Activated),
        minimized: t.state.contains(&ToplevelState::Minimized),
    }
}

impl App {
    fn active_workspaces(&self) -> Vec<ExtWorkspaceHandleV1> {
        self.workspace_state
            .workspaces()
            .filter(|w| w.state.contains(ext_workspace_handle_v1::State::Active))
            .map(|w| w.handle.clone())
            .collect()
    }

    fn current_workspace(&self) -> Option<ExtWorkspaceHandleV1> {
        let focused: Option<Vec<ExtWorkspaceHandleV1>> = self
            .toplevel_info_state
            .toplevels()
            .find(|t| t.state.contains(&ToplevelState::Activated))
            .map(|t| t.workspace.iter().cloned().collect());
        model::current_workspace(&self.active_workspaces(), focused.as_deref())
    }

    fn output_name(&self, output: &WlOutput) -> String {
        self.output_state
            .info(output)
            .and_then(|i| i.name)
            .unwrap_or_else(|| "?".into())
    }

    fn snapshot(&self) -> Snapshot {
        let current = self.current_workspace();
        let mut workspaces = Vec::new();
        let mut groups: Vec<_> = self.workspace_state.workspace_groups().collect();
        groups.sort_by_key(|g| g.outputs.first().map(|o| self.output_name(o)));
        for group in groups {
            let mut list: Vec<_> = self
                .workspace_state
                .workspaces()
                .filter(|w| group.workspaces.contains(&w.handle))
                .collect();
            list.sort_by(|a, b| a.coordinates.cmp(&b.coordinates));
            for ws in list {
                workspaces.push(WorkspaceView {
                    name: ws.name.clone(),
                    outputs: group.outputs.iter().map(|o| self.output_name(o)).collect(),
                    active: ws.state.contains(ext_workspace_handle_v1::State::Active),
                    current: current.as_ref() == Some(&ws.handle),
                    windows: self
                        .toplevel_info_state
                        .toplevels()
                        .filter(|t| t.workspace.contains(&ws.handle))
                        .map(window_view)
                        .collect(),
                });
            }
        }
        let known: Vec<_> = self.workspace_state.workspaces().map(|w| w.handle.clone()).collect();
        let other_windows = self
            .toplevel_info_state
            .toplevels()
            .filter(|t| !t.workspace.iter().any(|w| known.contains(w)))
            .map(window_view)
            .collect();
        Snapshot {
            workspaces,
            other_windows,
        }
    }

    fn print_if_changed(&mut self) {
        let snap = self.snapshot();
        let mut out = String::new();
        for ws in &snap.workspaces {
            out += &format!(
                "{} workspace {:?} on {}{}\n",
                if ws.current { "▶" } else if ws.active { "•" } else { " " },
                ws.name,
                ws.outputs.join(","),
                if ws.active { " (active)" } else { "" },
            );
            for w in &ws.windows {
                out += &format!("      {}\n", fmt_window(w));
            }
        }
        for w in &snap.other_windows {
            out += &format!("  (no workspace) {}\n", fmt_window(w));
        }
        if out != self.last_printed {
            println!("──── {} workspaces ────\n{out}", snap.workspaces.len());
            self.last_printed = out;
        }
    }

    fn switch(&mut self, dir: Direction) -> Result<String> {
        let ws = self
            .current_workspace()
            .ok_or_else(|| anyhow!("cannot tell which workspace is current"))?;
        let candidates: Vec<Candidate<String>> = self
            .toplevel_info_state
            .toplevels()
            .filter(|t| t.workspace.contains(&ws) && t.cosmic_toplevel.is_some())
            .map(|t| Candidate {
                key: t.identifier.clone(),
                focused: t.state.contains(&ToplevelState::Activated),
            })
            .collect();
        let Some(key) = self.switcher.pick(Instant::now(), dir, &ws.id(), &candidates) else {
            return Ok("ok nothing to switch to".into());
        };
        self.activate(&key)
    }

    fn activate(&self, identifier: &str) -> Result<String> {
        self.act(|t| t.identifier == identifier, Action::Activate)
    }

    /// Sends one manager request to the window matching `pick`.
    /// Fresh lookup: only handles cctk still lists (has cosmic info, not closed).
    fn act(&self, pick: impl Fn(&ToplevelInfo) -> bool, action: Action) -> Result<String> {
        let manager = self
            .toplevel_manager_state
            .as_ref()
            .context("toplevel manager not bound")?;
        let seat = self.seat_state.seats().next().context("no seat")?;
        let Some(info) = self.toplevel_info_state.toplevels().find(|t| pick(t)) else {
            return Ok("ok window already gone".into());
        };
        let Some(handle) = info.cosmic_toplevel.as_ref().filter(|h| h.is_alive()) else {
            return Ok("ok window has no cosmic handle".into());
        };
        match action {
            Action::Activate => manager.manager.activate(handle, &seat),
            Action::Close => manager.manager.close(handle),
        }
        self.conn.flush().context("flushing wayland connection")?;
        debug!("{action:?} {} ({})", info.app_id, info.title);
        Ok(format!("ok {}", info.app_id))
    }

    /// Windows for the launcher plugin, least recently used first (the order the stock
    /// cosmic_toplevel plugin emits; cosmic-launcher reverses it).
    /// An empty query lists the current workspace; a search lists matching windows everywhere.
    fn list(&self, query: &str) -> Vec<ListedWindow> {
        let query = query.to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let matches = |hay: &str| {
            let hay = hay.to_lowercase();
            words.iter().all(|w| hay.contains(w))
        };
        let scope: Vec<ExtWorkspaceHandleV1> = if words.is_empty() {
            // Unknown current workspace (several monitors, nothing focused): visible workspaces.
            self.current_workspace()
                .map(|ws| vec![ws])
                .unwrap_or_else(|| self.active_workspaces())
        } else {
            Vec::new()
        };
        let windows: Vec<&ToplevelInfo> = self
            .toplevel_info_state
            .toplevels()
            .filter(|t| t.cosmic_toplevel.is_some())
            .filter(|t| {
                if words.is_empty() {
                    t.workspace.iter().any(|w| scope.contains(w))
                } else {
                    matches(&t.app_id) || matches(&t.title)
                }
            })
            .collect();
        let candidates: Vec<Candidate<String>> = windows
            .iter()
            .map(|t| Candidate {
                key: t.identifier.clone(),
                focused: t.state.contains(&ToplevelState::Activated),
            })
            .collect();
        let mut order = self.switcher.ordered(&candidates);
        order.reverse();
        order
            .iter()
            .filter_map(|key| windows.iter().find(|t| &t.identifier == key))
            .map(|t| ListedWindow {
                id: t.foreign_toplevel.id().protocol_id(),
                app_id: t.app_id.clone(),
                title: t.title.clone(),
            })
            .collect()
    }

    fn handle_request(&mut self, line: &str) -> String {
        let result = match line {
            "switch next" => self.switch(Direction::Next),
            "switch prev" => self.switch(Direction::Prev),
            "status" => serde_json::to_string(&self.snapshot()).map_err(Into::into),
            "list" => serde_json::to_string(&self.list("")).map_err(Into::into),
            other => {
                let (cmd, arg) = other.split_once(' ').unwrap_or((other, ""));
                let id = || arg.parse::<u32>().with_context(|| format!("bad window id {arg:?}"));
                match cmd {
                    "list" => serde_json::to_string(&self.list(arg)).map_err(Into::into),
                    "activate" => id().and_then(|id| {
                        self.act(|t| t.foreign_toplevel.id().protocol_id() == id, Action::Activate)
                    }),
                    "close" => id().and_then(|id| {
                        self.act(|t| t.foreign_toplevel.id().protocol_id() == id, Action::Close)
                    }),
                    _ => Err(anyhow!("unknown command {other:?}")),
                }
            }
        };
        result.unwrap_or_else(|err| format!("err {err:#}"))
    }

    fn track_focus(&mut self, toplevel: &ExtForeignToplevelHandleV1) {
        if let Some(info) = self.toplevel_info_state.info(toplevel)
            && info.state.contains(&ToplevelState::Activated)
        {
            let id = info.identifier.clone();
            self.switcher.note_focused(&id);
        }
        self.dirty = true;
    }
}

fn fmt_window(w: &WindowView) -> String {
    format!(
        "{}{} {:?}{}",
        if w.focused { "* " } else { "" },
        w.app_id,
        w.title,
        if w.minimized { " [minimized]" } else { "" }
    )
}

pub struct Options {
    pub mode: Mode,
    pub burst: Duration,
}

pub fn run(opts: Options) -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn).context("reading Wayland globals")?;
    let qh = event_queue.handle();
    let mut event_loop = calloop::EventLoop::<App>::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .map_err(|e| anyhow!("inserting wayland source: {e}"))?;

    let registry_state = RegistryState::new(&globals);
    let toplevel_info_state = ToplevelInfoState::try_new(&registry_state, &qh)
        .context("compositor does not offer ext-foreign-toplevel-list")?;
    if toplevel_info_state.cosmic_toplevel_info.is_none() {
        return Err(anyhow!("compositor does not offer zcosmic-toplevel-info v2+"));
    }
    let toplevel_manager_state = match opts.mode {
        Mode::Watch => None,
        Mode::Daemon => Some(
            ToplevelManagerState::try_new(&registry_state, &qh)
                .context("compositor does not offer zcosmic-toplevel-manager")?,
        ),
    };
    let mut app = App {
        mode: opts.mode,
        conn: conn.clone(),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        workspace_state: WorkspaceState::new(&registry_state, &qh),
        toplevel_info_state,
        toplevel_manager_state,
        registry_state,
        switcher: Switcher::new(opts.burst),
        dirty: true,
        last_printed: String::new(),
    };

    if opts.mode == Mode::Daemon {
        let (tx, rx) = calloop::channel::channel::<crate::ipc::Request>();
        crate::ipc::serve(move |req| tx.send(req).is_ok())?;
        handle
            .insert_source(rx, |event, (), app| {
                if let calloop::channel::Event::Msg(req) = event {
                    let answer = app.handle_request(&req.line);
                    debug!("ipc {:?} -> {answer}", req.line);
                    let _ = req.reply.send(answer);
                }
            })
            .map_err(|e| anyhow!("inserting ipc source: {e}"))?;
        info!("daemon ready on {}", crate::ipc::socket_path()?.display());
    }

    loop {
        event_loop
            .dispatch(None, &mut app)
            .context("Wayland connection lost")?;
        if app.mode == Mode::Watch && app.dirty {
            app.dirty = false;
            app.print_if_changed();
        }
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers!(OutputState, SeatState);
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.dirty = true;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.dirty = true;
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.dirty = true;
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl WorkspaceHandler for App {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }
    fn done(&mut self) {
        self.dirty = true;
    }
}

impl ToplevelInfoHandler for App {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }
    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, toplevel: &ExtForeignToplevelHandleV1) {
        self.track_focus(toplevel);
    }
    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, toplevel: &ExtForeignToplevelHandleV1) {
        self.track_focus(toplevel);
    }
    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, toplevel: &ExtForeignToplevelHandleV1) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            let id = info.identifier.clone();
            self.switcher.forget(&id);
        }
        self.dirty = true;
    }
    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>) {
        error!("compositor stopped sending toplevel info");
    }
}

impl ToplevelManagerHandler for App {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        // Only reachable through events on the manager, which exists only in daemon mode.
        self.toplevel_manager_state.as_mut().expect("manager bound")
    }
    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        capabilities: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
        let can_activate = capabilities.iter().any(|c| {
            matches!(
                c,
                WEnum::Value(zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Activate)
            )
        });
        if !can_activate {
            warn!("compositor did not advertise the activate capability");
        }
    }
}

sctk::delegate_registry!(App);
sctk::delegate_output!(App);
sctk::delegate_seat!(App);
cctk::delegate_workspace!(App);
cctk::delegate_toplevel_info!(App);
cctk::delegate_toplevel_manager!(App);

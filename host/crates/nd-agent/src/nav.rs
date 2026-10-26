// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone navigation rail (round 10): handles `NavRequest` and keeps the phone's `NavState` current.
//!
//! Lock rules: requests are handled off the transport reactor. Home/Resume go through
//! [`ProfileActions`], which queue profile changes to the auto-switch command worker; Back takes the
//! `AgentState` write lock itself, on the blocking pool, and never from inside an action.

use crate::autoswitch::AutoSwitchService;
use crate::AgentState;
use nd_core::autoswitch::ProfileActions;
use nd_core::PauseMode;
use nd_proto::v1::{NavAction, NavRequest, NavState, SetLayout, SetRail};
use nd_proto::Msg;
use nd_transport::{ServerHandle, SessionHandle};
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};

pub struct Nav {
    state: AgentState,
    svc: Arc<AutoSwitchService>,
    server: ServerHandle,
    /// Last `NavState` broadcast, for de-duplicating change-only sends.
    last: Mutex<Option<NavState>>,
}

impl Nav {
    pub fn new(state: AgentState, svc: Arc<AutoSwitchService>, server: ServerHandle) -> Arc<Self> {
        Arc::new(Self { state, svc, server, last: Mutex::new(None) })
    }

    /// Current rail state. Blocking (takes the state read lock): call from a blocking context.
    pub fn compute(&self) -> NavState {
        let main = self.svc.main_profile_id();
        let snap = self.state.nav_snapshot(main.as_deref());
        NavState {
            can_go_back: snap.can_go_back,
            auto_switch_paused: self.svc.paused(),
            on_home: snap.on_home,
            profile_name: snap.profile_name,
            page_index: snap.page_index,
            page_count: snap.page_count,
        }
    }

    async fn compute_async(self: &Arc<Self>) -> Option<NavState> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.compute()).await.ok()
    }

    /// Broadcast a layout to the phones, then the matching `NavState` (always: the phone may reset its
    /// rail on a layout). Errors (no phone connected) are expected and only logged.
    pub async fn push_layout(self: &Arc<Self>, layout: SetLayout) {
        if let Err(e) = self.server.set_layout(layout).await {
            debug!("Layout push skipped: {e}");
            return;
        }
        self.publish(true).await;
    }

    /// Broadcast the current `NavState`; unless `force`, only when it differs from the last one sent.
    pub async fn publish(self: &Arc<Self>, force: bool) {
        let Some(ns) = self.compute_async().await else { return };
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if !force && last.as_ref() == Some(&ns) {
                return;
            }
            *last = Some(ns.clone());
        }
        for s in self.server.all_sessions() {
            if s.is_connected() {
                if let Err(e) = s.send_msg(Msg::NavState(ns.clone())).await {
                    debug!("NavState send skipped: {e}");
                }
            }
        }
    }

    /// `NavState` for one freshly connected session (sent right after its initial layout).
    pub async fn send_to(self: &Arc<Self>, session: &SessionHandle) {
        let Some(ns) = self.compute_async().await else { return };
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(ns.clone());
        if let Err(e) = session.send_msg(Msg::NavState(ns)).await {
            debug!("NavState send skipped: {e}");
        }
    }

    /// `SetRail` for one freshly connected session (sent right after its first layout).
    pub async fn send_rail_to(self: &Arc<Self>, session: &SessionHandle) {
        let rail = self.state.rail_proto();
        if let Err(e) = session.send_msg(Msg::SetRail(rail)).await {
            debug!("SetRail send skipped: {e}");
        }
    }

    /// Push a changed rail to every connected session.
    pub async fn broadcast_rail(self: &Arc<Self>, rail: SetRail) {
        for s in self.server.all_sessions() {
            if s.is_connected() {
                if let Err(e) = s.send_msg(Msg::SetRail(rail.clone())).await {
                    debug!("SetRail send skipped: {e}");
                }
            }
        }
    }

    /// Run a phone request. Resulting layout changes arrive through `StateEvent::LayoutChanged`
    /// (which pushes the layout and `NavState`); pure pause changes through [`Nav::publish`].
    pub async fn handle_request(self: &Arc<Self>, req: NavRequest) {
        let action = NavAction::try_from(req.action).unwrap_or(NavAction::Unspecified);
        let me = self.clone();
        let index = req.page_index as usize;
        let res = tokio::task::spawn_blocking(move || me.run_action(action, index)).await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("Navigation request {action:?} failed: {e}"),
            Err(e) => warn!("Navigation task failed: {e}"),
        }
        // Covers requests that change only the paused state (and no-ops); deduplicated.
        self.publish(false).await;
    }

    fn run_action(&self, action: NavAction, page_index: usize) -> Result<(), String> {
        match action {
            NavAction::Home => {
                if self.svc.paused() {
                    self.svc.set_paused(PauseMode::Resume).map(|_| ())
                } else {
                    self.svc.go_home(false)
                }
            }
            NavAction::HomePause => self.svc.go_home(true),
            NavAction::Back => self.state.nav_back().map(|_| ()).map_err(|e| e.to_string()),
            NavAction::GoToPage => self.state.nav_go_to_page(page_index).map(|_| ()).map_err(|e| e.to_string()),
            NavAction::Resume => self.svc.set_paused(PauseMode::Resume).map(|_| ()),
            NavAction::Unspecified => {
                debug!("Ignoring NavRequest with unspecified action");
                Ok(())
            }
        }
    }
}

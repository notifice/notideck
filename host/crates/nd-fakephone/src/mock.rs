// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;

use nd_proto::framing;
use nd_proto::v1::*;
use nd_proto::{Envelope, Msg};

use crate::error::FakePhoneError;

/// Action type simulated by the mock router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockAction {
    Hotkey(String),
    Launch(String),
    OpenUrl(String),
    Custom(String),
}

/// Recorded action execution event triggered by an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggeredAction {
    pub control_id: String,
    pub input_kind: i32,
    pub action: MockAction,
    pub time_ms: u64,
}

/// Mock transport server and core action router for testing.
pub struct MockHost {
    local_addr: SocketAddr,
    router: Arc<RwLock<HashMap<String, MockAction>>>,
    layout: Arc<RwLock<SetLayout>>,
    clients: Arc<RwLock<Vec<mpsc::Sender<Envelope>>>>,
    triggered_actions: Arc<RwLock<Vec<TriggeredAction>>>,
    triggered_actions_tx: broadcast::Sender<TriggeredAction>,
    received_inputs: Arc<RwLock<Vec<InputEvent>>>,
    received_inputs_tx: broadcast::Sender<InputEvent>,
    received_pongs: Arc<RwLock<Vec<Pong>>>,
    received_pongs_tx: broadcast::Sender<Pong>,
    received_health: Arc<RwLock<Vec<DeviceHealth>>>,
    received_health_tx: broadcast::Sender<DeviceHealth>,
    accept_handle: JoinHandle<()>,
    host_fp: nd_transport::Fingerprint,
    seq_counter: Arc<AtomicU32>,
}

fn default_mock_layout() -> SetLayout {
    SetLayout {
        page_id: "main".to_string(),
        cols: 5,
        rows: 3,
        controls: vec![
            Control {
                id: "btn-mute".to_string(),
                kind: ControlKind::Key as i32,
                col: 0,
                row: 0,
                span_w: 1,
                span_h: 1,
                title: "Mute".to_string(),
                ..Default::default()
            },
            Control {
                id: "btn-cam".to_string(),
                kind: ControlKind::Key as i32,
                col: 1,
                row: 0,
                span_w: 1,
                span_h: 1,
                title: "Camera".to_string(),
                ..Default::default()
            },
            Control {
                id: "dial-vol".to_string(),
                kind: ControlKind::Dial as i32,
                col: 2,
                row: 0,
                span_w: 1,
                span_h: 1,
                title: "Volume".to_string(),
                ..Default::default()
            },
            Control {
                id: "slider-mix".to_string(),
                kind: ControlKind::Slider as i32,
                col: 3,
                row: 0,
                span_w: 1,
                span_h: 2,
                title: "Master".to_string(),
                ..Default::default()
            },
        ],
    }
}

impl MockHost {
    /// Bind to ephemeral port `127.0.0.1:0`.
    pub async fn bind_ephemeral() -> Result<Self, FakePhoneError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        Self::bind(addr).await
    }

    /// Bind to default control port `127.0.0.1:47800`.
    pub async fn bind_default() -> Result<Self, FakePhoneError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], nd_proto::CONTROL_PORT));
        Self::bind(addr).await
    }

    /// Bind to a specific address.
    pub async fn bind(addr: SocketAddr) -> Result<Self, FakePhoneError> {
        let listener = TcpListener::bind(addr).await?;
        // Throw-away host identity; the mock trusts every phone (no pairing).
        let id_dir = std::env::temp_dir().join(format!("nd-mockhost-{}", uuid::Uuid::new_v4()));
        let identity =
            nd_transport::HostIdentity::load_or_create(&id_dir).map_err(|e| FakePhoneError::Tls(e.to_string()))?;
        let _ = std::fs::remove_dir_all(&id_dir);
        let tls_cfg = nd_transport::tls::server_config(&identity).map_err(|e| FakePhoneError::Tls(e.to_string()))?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_cfg));
        let host_id = identity.host_id().to_string();
        let host_fp = identity.fingerprint();
        let local_addr = listener.local_addr()?;

        let router = Arc::new(RwLock::new(HashMap::<String, MockAction>::new()));
        let layout = Arc::new(RwLock::new(default_mock_layout()));
        let clients = Arc::new(RwLock::new(Vec::new()));
        let triggered_actions = Arc::new(RwLock::new(Vec::new()));
        let (triggered_actions_tx, _) = broadcast::channel(128);
        let received_inputs = Arc::new(RwLock::new(Vec::new()));
        let (received_inputs_tx, _) = broadcast::channel(128);
        let received_pongs = Arc::new(RwLock::new(Vec::new()));
        let (received_pongs_tx, _) = broadcast::channel(128);
        let received_health = Arc::new(RwLock::new(Vec::new()));
        let (received_health_tx, _) = broadcast::channel(128);
        let seq_counter = Arc::new(AtomicU32::new(1));

        let accept_handle = tokio::spawn({
            let router = Arc::clone(&router);
            let layout = Arc::clone(&layout);
            let clients = Arc::clone(&clients);
            let triggered_actions = Arc::clone(&triggered_actions);
            let triggered_actions_tx = triggered_actions_tx.clone();
            let received_inputs = Arc::clone(&received_inputs);
            let received_inputs_tx = received_inputs_tx.clone();
            let received_pongs = Arc::clone(&received_pongs);
            let received_pongs_tx = received_pongs_tx.clone();
            let received_health = Arc::clone(&received_health);
            let received_health_tx = received_health_tx.clone();
            let seq_counter = Arc::clone(&seq_counter);
            let host_id = host_id.clone();

            async move {
                while let Ok((stream, _peer_addr)) = listener.accept().await {
                    let tls = match tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream)).await {
                        Ok(Ok(t)) => t,
                        _ => continue,
                    };
                    let (mut read_half, mut write_half) = tokio::io::split(tls);
                    let (client_tx, mut client_rx) = mpsc::channel::<Envelope>(64);

                    {
                        let mut cl = clients.write().await;
                        cl.push(client_tx.clone());
                    }

                    // Writer task for this client
                    tokio::spawn(async move {
                        while let Some(env) = client_rx.recv().await {
                            if let Err(e) = framing::write_frame(&mut write_half, &env).await {
                                tracing::debug!("mock host client write error: {e}");
                                break;
                            }
                        }
                    });

                    // Reader task for this client
                    let router = Arc::clone(&router);
                    let layout = Arc::clone(&layout);
                    let clients = Arc::clone(&clients);
                    let triggered_actions = Arc::clone(&triggered_actions);
                    let triggered_actions_tx = triggered_actions_tx.clone();
                    let received_inputs = Arc::clone(&received_inputs);
                    let received_inputs_tx = received_inputs_tx.clone();
                    let received_pongs = Arc::clone(&received_pongs);
                    let received_pongs_tx = received_pongs_tx.clone();
                    let received_health = Arc::clone(&received_health);
                    let received_health_tx = received_health_tx.clone();
                    let seq_counter = Arc::clone(&seq_counter);
                    let client_tx_clone = client_tx.clone();
                    let host_id = host_id.clone();

                    tokio::spawn(async move {
                        // 1. Expect Hello first
                        let hello_env = match framing::read_frame(&mut read_half).await {
                            Ok(Some(env)) => env,
                            _ => return,
                        };

                        if let Some(Msg::Hello(_)) = hello_env.msg {
                            // 2. Send Welcome
                            let welcome_seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                            let welcome = Welcome {
                                session_id: uuid::Uuid::new_v4().to_string(),
                                host_name: "MockNotideckHost".to_string(),
                                proto_version: nd_proto::PROTO_VERSION,
                                host_id: host_id.clone(),
                            };
                            let _ = client_tx_clone.send(Envelope::new(welcome_seq, Msg::Welcome(welcome))).await;

                            // 3. Send SetLayout
                            let initial_layout = layout.read().await.clone();
                            let layout_seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                            let _ =
                                client_tx_clone.send(Envelope::new(layout_seq, Msg::SetLayout(initial_layout))).await;
                        } else {
                            return;
                        }

                        // 4. Handle incoming messages
                        loop {
                            match framing::read_frame(&mut read_half).await {
                                Ok(Some(env)) => {
                                    if let Some(msg) = env.msg {
                                        match msg {
                                            Msg::Input(input) => {
                                                {
                                                    let mut ins = received_inputs.write().await;
                                                    ins.push(input.clone());
                                                }
                                                let _ = received_inputs_tx.send(input.clone());

                                                // Route action if registered
                                                let router_guard = router.read().await;
                                                if let Some(action) = router_guard.get(&input.control_id) {
                                                    let trig = TriggeredAction {
                                                        control_id: input.control_id.clone(),
                                                        input_kind: input.kind,
                                                        action: action.clone(),
                                                        time_ms: input.phone_time_ms,
                                                    };
                                                    {
                                                        let mut acts = triggered_actions.write().await;
                                                        acts.push(trig.clone());
                                                    }
                                                    let _ = triggered_actions_tx.send(trig);
                                                }
                                            }
                                            Msg::Pong(pong) => {
                                                {
                                                    let mut pongs = received_pongs.write().await;
                                                    pongs.push(pong);
                                                }
                                                let _ = received_pongs_tx.send(pong);
                                            }
                                            Msg::Health(health) => {
                                                {
                                                    let mut hs = received_health.write().await;
                                                    hs.push(health);
                                                }
                                                let _ = received_health_tx.send(health);
                                            }
                                            Msg::Ping(ping) => {
                                                let pong_seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                                                let _ = client_tx_clone
                                                    .send(Envelope::new(
                                                        pong_seq,
                                                        Msg::Pong(Pong { nonce: ping.nonce, host_time_ms: 0 }),
                                                    ))
                                                    .await;
                                            }
                                            Msg::Goodbye(_) => {
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                Ok(None) => break,
                                Err(_) => break,
                            }
                        }

                        // Remove client on disconnect
                        let mut cl = clients.write().await;
                        cl.retain(|tx| !tx.same_channel(&client_tx_clone));
                    });
                }
            }
        });

        Ok(MockHost {
            local_addr,
            router,
            layout,
            clients,
            triggered_actions,
            triggered_actions_tx,
            received_inputs,
            received_inputs_tx,
            received_pongs,
            received_pongs_tx,
            received_health,
            received_health_tx,
            accept_handle,
            host_fp,
            seq_counter,
        })
    }

    /// Fingerprint of the mock host's TLS certificate.
    pub fn host_fp(&self) -> nd_transport::Fingerprint {
        self.host_fp
    }

    /// Local bound address of the server.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Local bound TCP port of the server.
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Register an action for a control ID in the mock core router.
    pub async fn register_action(&self, control_id: &str, action: MockAction) {
        let mut r = self.router.write().await;
        r.insert(control_id.to_string(), action);
    }

    /// Update the current deck layout stored on host.
    pub async fn set_layout(&self, layout: SetLayout) {
        let mut l = self.layout.write().await;
        *l = layout;
    }

    /// Push an updated `SetLayout` to all connected clients.
    pub async fn send_layout(&self, layout: SetLayout) -> Result<(), FakePhoneError> {
        self.set_layout(layout.clone()).await;
        self.send_to_clients(Msg::SetLayout(layout)).await
    }

    /// Push `UpdateControls` patches to all connected clients.
    pub async fn send_update_controls(&self, update: UpdateControls) -> Result<(), FakePhoneError> {
        self.send_to_clients(Msg::UpdateControls(update)).await
    }

    /// Send `Feedback` to all connected clients.
    pub async fn send_feedback(&self, feedback: Feedback) -> Result<(), FakePhoneError> {
        self.send_to_clients(Msg::Feedback(feedback)).await
    }

    /// Send a `Ping` to all connected clients.
    pub async fn send_ping(&self, nonce: u64) -> Result<(), FakePhoneError> {
        self.send_to_clients(Msg::Ping(Ping { nonce })).await
    }

    async fn send_to_clients(&self, msg: Msg) -> Result<(), FakePhoneError> {
        let seq = self.seq_counter.fetch_add(1, Ordering::SeqCst);
        let env = Envelope::new(seq, msg);
        let cl = self.clients.read().await;
        for tx in cl.iter() {
            let _ = tx.send(env.clone()).await;
        }
        Ok(())
    }

    /// Wait for an action to be triggered for `control_id` within `timeout`.
    pub async fn wait_for_action(
        &self,
        control_id: &str,
        timeout: Duration,
    ) -> Result<TriggeredAction, FakePhoneError> {
        {
            let actions = self.triggered_actions.read().await;
            if let Some(act) = actions.iter().find(|a| a.control_id == control_id) {
                return Ok(act.clone());
            }
        }

        let mut rx = self.triggered_actions_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("TriggeredAction"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(act)) if act.control_id == control_id => return Ok(act),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    let actions = self.triggered_actions.read().await;
                    if let Some(act) = actions.iter().find(|a| a.control_id == control_id) {
                        return Ok(act.clone());
                    }
                }
                Err(_) => return Err(FakePhoneError::Timeout("TriggeredAction")),
            }
        }
    }

    /// Wait for an `InputEvent` matching a predicate within `timeout`.
    pub async fn wait_for_input_matching<F>(
        &self,
        timeout: Duration,
        predicate: F,
    ) -> Result<InputEvent, FakePhoneError>
    where
        F: Fn(&InputEvent) -> bool,
    {
        {
            let inputs = self.received_inputs.read().await;
            if let Some(inp) = inputs.iter().find(|i| predicate(i)) {
                return Ok(inp.clone());
            }
        }

        let mut rx = self.received_inputs_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("InputEvent"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(inp)) if predicate(&inp) => return Ok(inp),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    let inputs = self.received_inputs.read().await;
                    if let Some(inp) = inputs.iter().find(|i| predicate(i)) {
                        return Ok(inp.clone());
                    }
                }
                Err(_) => return Err(FakePhoneError::Timeout("InputEvent")),
            }
        }
    }

    /// Wait for a `Pong` with matching nonce.
    pub async fn wait_for_pong(&self, nonce: u64, timeout: Duration) -> Result<Pong, FakePhoneError> {
        {
            let pongs = self.received_pongs.read().await;
            if let Some(p) = pongs.iter().find(|p| p.nonce == nonce) {
                return Ok(*p);
            }
        }

        let mut rx = self.received_pongs_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("Pong"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(p)) if p.nonce == nonce => return Ok(p),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    let pongs = self.received_pongs.read().await;
                    if let Some(p) = pongs.iter().find(|p| p.nonce == nonce) {
                        return Ok(*p);
                    }
                }
                Err(_) => return Err(FakePhoneError::Timeout("Pong")),
            }
        }
    }

    /// Wait for a `DeviceHealth` report.
    pub async fn wait_for_health(&self, timeout: Duration) -> Result<DeviceHealth, FakePhoneError> {
        {
            let healths = self.received_health.read().await;
            if let Some(h) = healths.last() {
                return Ok(*h);
            }
        }

        let mut rx = self.received_health_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("DeviceHealth"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(h)) => return Ok(h),
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    let healths = self.received_health.read().await;
                    if let Some(h) = healths.last() {
                        return Ok(*h);
                    }
                }
                Err(_) => return Err(FakePhoneError::Timeout("DeviceHealth")),
            }
        }
    }

    /// Inspect all received device health reports.
    pub async fn received_health(&self) -> Vec<DeviceHealth> {
        self.received_health.read().await.clone()
    }

    /// Inspect all triggered actions history.
    pub async fn triggered_actions(&self) -> Vec<TriggeredAction> {
        self.triggered_actions.read().await.clone()
    }

    /// Inspect all raw input events received.
    pub async fn received_inputs(&self) -> Vec<InputEvent> {
        self.received_inputs.read().await.clone()
    }

    /// Count of currently connected clients.
    pub async fn connected_clients_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Terminate listener.
    pub fn stop(self) {
        self.accept_handle.abort();
    }
}

impl Drop for MockHost {
    fn drop(&mut self) {
        self.accept_handle.abort();
    }
}

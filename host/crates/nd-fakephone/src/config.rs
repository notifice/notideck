// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::time::Duration;

use nd_proto::v1::{Capability, ScreenInfo};
use nd_proto::PROTO_VERSION;
use nd_transport::pairing::SECRET_LEN;
use nd_transport::Fingerprint;

use crate::store::ClientIdentity;

/// Callback invoked with the host name when the host answers `PairingPending`.
#[derive(Clone)]
pub struct PendingHook(pub Arc<dyn Fn(&str) + Send + Sync>);

impl std::fmt::Debug for PendingHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingHook")
    }
}

/// Configuration options for the fake phone test client.
#[derive(Debug, Clone)]
pub struct FakePhoneConfig {
    /// Protocol version advertised in Hello (defaults to `nd_proto::PROTO_VERSION`).
    pub proto_version: u32,
    /// Stable mock device identifier.
    pub device_id: String,
    /// Mock device model string (e.g. manufacturer + model).
    pub device_model: String,
    /// User-visible phone name sent in `Hello.device_name` and `PairRequest.device_name`.
    pub device_name: String,
    /// Mock Android SDK API level.
    pub android_sdk: u32,
    /// App version string.
    pub app_version: String,
    /// Mock screen display dimensions and DPI.
    pub screen: ScreenInfo,
    /// Hardware capabilities advertised to the host.
    pub capabilities: Vec<Capability>,
    /// Maximum duration to wait during handshake for Welcome (and SetLayout if `require_layout`).
    pub handshake_timeout: Duration,
    /// How long to keep waiting after `PairingPending` for the user to press Allow on the PC.
    pub approval_timeout: Duration,
    /// Whether to automatically respond to Ping messages with Pong.
    pub auto_pong: bool,
    /// Whether `connect` also waits for the first `SetLayout` after `Welcome`. Real hosts push the
    /// layout from the agent, a bare transport server does not, so tests against one turn this off.
    pub require_layout: bool,
    /// TLS client identity. `None` generates a throw-away one per connection.
    pub identity: Option<Arc<ClientIdentity>>,
    /// Pinned host certificate fingerprint. `None` = trust on first use.
    pub pin: Option<Fingerprint>,
    /// QR pairing secret (from the pairing URI). When set a `PairRequest` follows the Hello.
    pub pair_secret: Option<[u8; SECRET_LEN]>,
    /// Called when the host reports the phone is waiting for approval on the PC.
    pub on_pending: Option<PendingHook>,
}

impl Default for FakePhoneConfig {
    fn default() -> Self {
        Self {
            proto_version: PROTO_VERSION,
            device_id: "fake-phone-test-client-1".to_string(),
            device_model: "Google Pixel 8 Pro (Fake)".to_string(),
            device_name: "Fake Phone".to_string(),
            android_sdk: 34,
            app_version: "0.1.0-test".to_string(),
            screen: ScreenInfo { width_px: 1080, height_px: 2400, dpi: 420 },
            capabilities: vec![Capability::Camera, Capability::Haptics, Capability::Torch, Capability::Player],
            handshake_timeout: Duration::from_secs(5),
            approval_timeout: Duration::from_secs(130),
            auto_pong: true,
            require_layout: true,
            identity: None,
            pin: None,
            pair_secret: None,
            on_pending: None,
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use nd_proto::framing::{read_frame, write_frame};
use nd_proto::v1::{Hello, ScreenInfo};
use nd_proto::{Envelope, Msg, PROTO_VERSION};
use nd_transport::tls::{client_config, generate_client_identity};
use nd_transport::{
    ControlServer, Fingerprint, HostIdentity, Link, MemoryTrustStore, PeerInfo, ServerHandle, TrustStore,
};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub type Stream = TlsStream<TcpStream>;

pub fn always_lan(_: &SocketAddr) -> Link {
    Link::Lan
}

pub struct Harness {
    pub addr: SocketAddr,
    pub handle: ServerHandle,
    pub identity: Arc<HostIdentity>,
    pub trust: Arc<MemoryTrustStore>,
    pub host_fp: Fingerprint,
    pub input_rx: mpsc::Receiver<nd_proto::v1::InputEvent>,
    pub media_rx: mpsc::Receiver<nd_transport::MediaStream>,
    pub shutdown: watch::Sender<bool>,
    dir: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Start a TLS server on 127.0.0.1:0. `lan` makes every peer count as a LAN phone.
pub async fn start(lan: bool) -> Harness {
    let dir = std::env::temp_dir().join(format!("nd-transport-test-{}", uuid::Uuid::new_v4()));
    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let host_fp = identity.fingerprint();
    let trust = Arc::new(MemoryTrustStore::new(host_fp));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    if lan {
        server.set_link_classifier(always_lan);
    }
    assert_eq!(server.host_fp(), host_fp);
    let addr = server.local_addr();
    let handle = server.handle();
    let input_rx = server.take_input_receiver().unwrap();
    let media_rx = server.take_media_receiver().unwrap();
    let (_task, shutdown) = server.spawn();
    Harness { addr, handle, identity, trust, host_fp, input_rx, media_rx, shutdown, dir }
}

pub struct Phone {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
    pub fp: Fingerprint,
}

impl Phone {
    pub fn new(name: &str) -> Self {
        let (cert, key) = generate_client_identity(name).unwrap();
        let fp = Fingerprint::of_der(&cert);
        Phone { cert, key, fp }
    }

    pub fn peer_info(&self) -> PeerInfo {
        PeerInfo {
            fp: self.fp,
            device_id: "pre-pinned".into(),
            device_name: "Pre-pinned".into(),
            device_model: "Test".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            link: Link::Usb,
        }
    }

    pub async fn connect(&self, addr: SocketAddr, pin: Option<Fingerprint>) -> std::io::Result<Stream> {
        connect_with(addr, self.cert.clone(), self.key.clone(), pin).await
    }
}

pub async fn connect_with(
    addr: SocketAddr,
    cert: Vec<u8>,
    key: Vec<u8>,
    pin: Option<Fingerprint>,
) -> std::io::Result<Stream> {
    let cfg = client_config(cert, key, pin).map_err(std::io::Error::other)?;
    let tcp = TcpStream::connect(addr).await?;
    let name = ServerName::try_from("notideck.local").unwrap();
    TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await
}

pub fn hello(device_id: &str) -> Hello {
    Hello {
        proto_version: PROTO_VERSION,
        device_id: device_id.into(),
        device_model: "Pixel 6".into(),
        android_sdk: 34,
        app_version: "2.0.0".into(),
        screen: Some(ScreenInfo { width_px: 1080, height_px: 2400, dpi: 400 }),
        capabilities: vec![1, 2],
        cert_fp: String::new(),
        device_name: String::new(),
    }
}

pub async fn send<W: tokio::io::AsyncWrite + Unpin>(s: &mut W, msg: Msg) {
    write_frame(s, &Envelope::new(1, msg)).await.unwrap();
}

/// Next message within 5 s. `None` on clean close or error.
pub async fn recv<R: tokio::io::AsyncRead + Unpin>(s: &mut R) -> Option<Msg> {
    match tokio::time::timeout(Duration::from_secs(5), read_frame(s)).await {
        Ok(Ok(Some(env))) => env.msg,
        Ok(_) => None,
        Err(_) => panic!("timed out waiting for a frame"),
    }
}

/// Next message, panicking on close.
pub async fn expect<R: tokio::io::AsyncRead + Unpin>(s: &mut R) -> Msg {
    recv(s).await.expect("connection closed unexpectedly")
}

/// Skip pings (the heartbeat may interleave) until a non-ping arrives.
pub async fn expect_skip_ping<R: tokio::io::AsyncRead + Unpin>(s: &mut R) -> Msg {
    loop {
        match expect(s).await {
            Msg::Ping(_) => continue,
            other => return other,
        }
    }
}

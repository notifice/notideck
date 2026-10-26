// SPDX-License-Identifier: GPL-3.0-or-later
//! SDK for writing Notideck plugins in Rust (protocol in `docs/PLUGINS.md`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), nd_plugin_sdk::Error> {
//! use nd_plugin_sdk::{Config, Event, Session};
//! let Session { out, mut events, .. } = Session::connect(Config::from_process()?).await?;
//! let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
//! loop {
//!     tokio::select! {
//!         ev = events.recv() => match ev {
//!             Some(Event::KeyDown(t)) => out.show_ok(&t.context),
//!             Some(_) => {}
//!             None => break, // socket closed: exit
//!         },
//!         _ = tick.tick() => {}
//!     }
//! }
//! # Ok(()) }
//! ```

mod config;
mod event;
mod outbox;
mod session;
pub mod settings;

pub use config::Config;
pub use event::{Appear, Event, Target};
pub use outbox::{Outbox, PhonePlay};
pub use session::{Events, Session};

/// SDK errors (startup only; after registration the run loop just ends when the socket closes).
#[derive(Debug)]
pub enum Error {
    /// Missing or malformed `--port/--token/--plugin-id`.
    Config(String),
    /// Connection or WebSocket failure.
    Connect(String),
    /// The agent rejected or did not answer `register`.
    Register(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Config(m) => write!(f, "config: {m}"),
            Error::Connect(m) => write!(f, "connect: {m}"),
            Error::Register(m) => write!(f, "register: {m}"),
        }
    }
}

impl std::error::Error for Error {}

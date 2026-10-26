// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::event::Event;
use crate::outbox::Outbox;
use crate::{Config, Error};

/// Stream of agent events. `recv` returns `None` once the socket closed: the plugin should exit.
pub struct Events {
    rx: mpsc::UnboundedReceiver<Event>,
}

impl Events {
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// A registered connection.
pub struct Session {
    pub out: Outbox,
    pub events: Events,
    /// The agent's `register` result (`{protocol, agent: {version, os}}`).
    pub agent: Value,
}

impl Session {
    /// Connect to `ws://127.0.0.1:<port>/`, send `register` and wait (up to 10 s) for the reply.
    /// A background task then pumps messages in both directions.
    pub async fn connect(cfg: Config) -> Result<Session, Error> {
        let url = format!("ws://127.0.0.1:{}/", cfg.port);
        let (mut ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&url))
            .await
            .map_err(|_| Error::Connect("timeout".into()))?
            .map_err(|e| Error::Connect(e.to_string()))?;

        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "register",
            "params": {"pluginId": cfg.plugin_id, "token": cfg.token, "protocol": 1}});
        ws.send(Message::text(req.to_string())).await.map_err(|e| Error::Register(e.to_string()))?;

        let agent = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(msg) = ws.next().await {
                let msg = msg.map_err(|e| Error::Register(e.to_string()))?;
                let Message::Text(text) = msg else {
                    if matches!(msg, Message::Close(_)) {
                        break;
                    }
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if v.get("id") != Some(&json!(1)) {
                    continue;
                }
                if let Some(e) = v.get("error") {
                    return Err(Error::Register(e.to_string()));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            Err(Error::Register("connection closed".into()))
        })
        .await
        .map_err(|_| Error::Register("no reply within 10 s".into()))??;

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
        tokio::spawn(async move {
            let (mut sink, mut stream) = ws.split();
            loop {
                tokio::select! {
                    msg = out_rx.recv() => match msg {
                        Some(m) => if sink.send(Message::text(m)).await.is_err() { break },
                        None => break,
                    },
                    msg = stream.next() => match msg {
                        Some(Ok(Message::Text(t))) => {
                            if let Some(ev) = decode_frame(&t) {
                                if ev_tx.send(ev).is_err() { break }
                            }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {}
                    },
                }
            }
            let _ = sink.close().await;
            // dropping ev_tx ends `Events::recv`
        });
        Ok(Session { out: Outbox::new(out_tx), events: Events { rx: ev_rx }, agent })
    }
}

/// Decode one text frame into an event (notifications only; responses are ignored).
pub(crate) fn decode_frame(text: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(text).ok()?;
    let method = v.get("method")?.as_str()?;
    Event::decode(method, v.get("params").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn frame_decoding() {
        let f = r#"{"jsonrpc":"2.0","method":"key.down","params":{"context":"c","action":"a","settings":{}}}"#;
        assert!(matches!(decode_frame(f), Some(Event::KeyDown(_))));
        assert!(decode_frame(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#).is_none());
        assert!(decode_frame("garbage").is_none());
    }

    #[tokio::test]
    async fn loopback_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let agent = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            let Message::Text(t) = ws.next().await.unwrap().unwrap() else { panic!() };
            let reg: Value = serde_json::from_str(&t).unwrap();
            assert_eq!(reg["method"], "register");
            assert_eq!(reg["params"], json!({"pluginId": "dev.t", "token": "tok", "protocol": 1}));
            let reply =
                json!({"jsonrpc":"2.0","id":reg["id"],"result":{"protocol":1,"agent":{"version":"0","os":"linux"}}});
            ws.send(Message::text(reply.to_string())).await.unwrap();
            let appear = json!({"jsonrpc":"2.0","method":"action.willAppear","params":
                {"context":"c1","action":"clock","settings":{"a":1},"controller":"key","state":0,"value":0}});
            ws.send(Message::text(appear.to_string())).await.unwrap();
            let Message::Text(t) = ws.next().await.unwrap().unwrap() else { panic!() };
            let m: Value = serde_json::from_str(&t).unwrap();
            assert_eq!(m["method"], "setTitle");
            assert_eq!(m["params"], json!({"context": "c1", "title": "hi"}));
            ws.close(None).await.unwrap();
        });

        let cfg = Config { port, token: "tok".into(), plugin_id: "dev.t".into() };
        let Session { out, mut events, agent: info } = Session::connect(cfg).await.unwrap();
        assert_eq!(info["agent"]["os"], "linux");
        match events.recv().await {
            Some(Event::WillAppear(a)) => {
                assert_eq!(a.target.context, "c1");
                out.set_title(&a.target.context, "hi");
            }
            e => panic!("{e:?}"),
        }
        assert!(events.recv().await.is_none(), "closed socket ends the stream");
        agent.await.unwrap();
    }

    #[tokio::test]
    async fn register_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            let _ = ws.next().await;
            let _ = ws.close(None).await;
        });
        let cfg = Config { port, token: "bad".into(), plugin_id: "x".into() };
        assert!(matches!(Session::connect(cfg).await, Err(Error::Register(_))));
    }
}

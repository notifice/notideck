// SPDX-License-Identifier: GPL-3.0-or-later
//! The local WebSocket server and the plugin → agent JSON-RPC methods (docs/PLUGINS.md §3-4).

use crate::host::HostEvent;
use crate::image::{self, ImageError};
use crate::manifest::PROTOCOL_VERSION;
use crate::state::{Look, Out, Shared};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

pub(crate) const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const ERR_METHOD: i64 = -32601;
pub(crate) const ERR_PARAMS: i64 = -32602;
pub(crate) const ERR_CONTEXT: i64 = -32001;
pub(crate) const ERR_NOT_ALLOWED: i64 = -32002;

type RpcResult = Result<Value, (i64, String)>;

pub(crate) async fn serve(shared: Arc<Shared>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let shared = shared.clone();
                tokio::spawn(async move { handle_conn(shared, stream).await });
            }
            Err(e) => {
                tracing::warn!(target: "plugin", error = %e, "plugin server accept failed");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn response(id: &Value, result: RpcResult) -> String {
    match result {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}).to_string(),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
        }
    }
}

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

async fn read_register(ws: &mut Ws) -> Option<Value> {
    loop {
        match ws.next().await? {
            Ok(Message::Text(t)) => return serde_json::from_str(&t).ok(),
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            _ => return None,
        }
    }
}

async fn handle_conn(shared: Arc<Shared>, stream: TcpStream) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
    let Ok(Some(first)) = tokio::time::timeout(REGISTER_TIMEOUT, read_register(&mut ws)).await else {
        let _ = ws.close(None).await;
        return;
    };
    let req_id = first.get("id").cloned().filter(|v| !v.is_null());
    let params = first.get("params").cloned().unwrap_or(Value::Null);
    let plugin_id = params.get("pluginId").and_then(Value::as_str).unwrap_or("").to_string();
    let token = params.get("token").and_then(Value::as_str).unwrap_or("");
    let protocol_ok = params.get("protocol").and_then(Value::as_u64) == Some(PROTOCOL_VERSION as u64);
    let is_register = first.get("method").and_then(Value::as_str) == Some("register");

    let token_ok = {
        let st = shared.lock();
        st.plugins.get(&plugin_id).and_then(|e| e.token.as_deref()).is_some_and(|t| ct_eq(t, token))
    };
    if !is_register || !token_ok || !protocol_ok {
        if let Some(id) = &req_id {
            let msg = if !is_register {
                "first message must be register"
            } else if !token_ok {
                "bad plugin id or token"
            } else {
                "unsupported protocol"
            };
            let code = if !is_register {
                ERR_METHOD
            } else if !token_ok {
                ERR_NOT_ALLOWED
            } else {
                ERR_PARAMS
            };
            let _ = ws.send(Message::Text(response(id, Err((code, msg.into()))))).await;
        }
        tracing::warn!(target: "plugin", plugin = %plugin_id, "plugin registration refused");
        let _ = ws.close(None).await;
        return;
    }

    let reply = req_id.as_ref().map(|id| response(id, Ok(shared.protocol_reply())));
    let Some((conn_id, mut rx)) = shared.attach(&plugin_id, reply) else {
        let _ = ws.close(None).await;
        return;
    };

    loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Text(t))) => handle_text(&shared, &plugin_id, conn_id, &t).await,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Some(Out::Text(t)) => {
                    if ws.send(Message::Text(t)).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close) | None => {
                    let _ = ws.close(None).await;
                    break;
                }
            },
        }
    }
    shared.detach(&plugin_id, conn_id);
}

async fn handle_text(shared: &Arc<Shared>, plugin: &str, conn_id: u64, text: &str) {
    let reply = |s: String| {
        let st = shared.lock();
        if let Some(c) = st.plugins.get(plugin).and_then(|e| e.conn.as_ref()).filter(|c| c.id == conn_id) {
            let _ = c.tx.send(Out::Text(s));
        }
    };
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            reply(response(&Value::Null, Err((-32700, format!("parse error: {e}")))));
            return;
        }
    };
    let id = v.get("id").cloned().filter(|i| !i.is_null());
    let Some(method) = v.get("method").and_then(Value::as_str) else { return };
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    let result = dispatch(shared, plugin, method, &params).await;
    match (&id, &result) {
        (Some(id), _) => reply(response(id, result)),
        (None, Err((code, msg))) => {
            tracing::debug!(target: "plugin", plugin, method, code, "notification failed: {msg}");
        }
        _ => {}
    }
}

fn str_param<'a>(p: &'a Value, key: &str) -> Result<&'a str, (i64, String)> {
    p.get(key).and_then(Value::as_str).ok_or((ERR_PARAMS, format!("missing or invalid {key:?}")))
}

fn ok() -> RpcResult {
    Ok(json!({}))
}

async fn dispatch(shared: &Arc<Shared>, plugin: &str, method: &str, p: &Value) -> RpcResult {
    match method {
        "setDetail" => {
            let ctx = str_param(p, "context")?;
            let val = str_param(p, "detail")?.to_string();
            update_look(shared, plugin, ctx, |l| l.detail = Some(val))?;
            ok()
        }
        "phone.play" => {
            let url = str_param(p, "url")?;
            if !url.to_ascii_lowercase().starts_with("https://") {
                return Err((ERR_NOT_ALLOWED, "only https stream URLs can be played".into()));
            }
            let opt = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let context = p.get("context").and_then(Value::as_str).map(str::to_string);
            if let Some(ctx) = &context {
                check_context(shared, plugin, ctx)?;
            }
            let max_height =
                p.get("maxHeight").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
            let sync_delay_ms = p.get("syncDelayMs").and_then(Value::as_u64).map(|n| n.min(60_000) as u32).unwrap_or(0);
            let _ = shared.events.send(HostEvent::PlayOnPhone {
                plugin: plugin.to_string(),
                context,
                url: url.to_string(),
                title: opt("title"),
                subtitle: opt("subtitle"),
                max_height,
                muted: p.get("muted").and_then(Value::as_bool).unwrap_or(false),
                sync_delay_ms,
                session: opt("session"),
            });
            ok()
        }
        "phone.stop" => {
            let _ = shared.events.send(HostEvent::StopOnPhone);
            ok()
        }
        "setTitle" | "setSubtitle" => {
            let ctx = str_param(p, "context")?;
            let (key, field) = if method == "setTitle" { ("title", 0) } else { ("subtitle", 1) };
            let val = str_param(p, key)?.to_string();
            update_look(
                shared,
                plugin,
                ctx,
                |l| if field == 0 { l.title = Some(val) } else { l.subtitle = Some(val) },
            )?;
            ok()
        }
        "setState" => {
            let ctx = str_param(p, "context")?;
            let n = p.get("state").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
            let n = n.ok_or((ERR_PARAMS, "state must be a non-negative integer".to_string()))?;
            update_look(shared, plugin, ctx, |l| l.state = Some(n))?;
            ok()
        }
        "setValue" => {
            let ctx = str_param(p, "context")?;
            let v = p.get("value").and_then(Value::as_f64).filter(|v| v.is_finite());
            let v = v.ok_or((ERR_PARAMS, "value must be a number".to_string()))?;
            let v = v.clamp(0.0, 1.0) as f32;
            update_look(shared, plugin, ctx, |l| l.value = Some(v))?;
            ok()
        }
        "setImage" => {
            let ctx = str_param(p, "context")?;
            let img = str_param(p, "image")?.to_string();
            check_context(shared, plugin, ctx)?;
            let hash = if img.is_empty() {
                String::new()
            } else {
                let sh = shared.clone();
                let plugin_owned = plugin.to_string();
                tokio::task::spawn_blocking(move || import_image(&sh, &plugin_owned, &img))
                    .await
                    .map_err(|e| (ERR_PARAMS, format!("image task failed: {e}")))??
            };
            update_look(shared, plugin, ctx, |l| l.icon_hash = Some(hash))?;
            ok()
        }
        "showOk" | "showAlert" => {
            let ctx = str_param(p, "context")?;
            check_context(shared, plugin, ctx)?;
            let _ = shared.events.send(HostEvent::Feedback { context: ctx.to_string(), ok: method == "showOk" });
            ok()
        }
        "setSettings" => {
            let ctx = str_param(p, "context")?;
            let settings = p
                .get("settings")
                .and_then(Value::as_object)
                .cloned()
                .ok_or((ERR_PARAMS, "settings must be an object".to_string()))?;
            {
                let mut st = shared.lock();
                check_ctx(&st, plugin, ctx)?;
                if let Some(v) = st.visible.get_mut(ctx) {
                    v.inst.settings = settings.clone();
                }
            }
            let _ = shared.events.send(HostEvent::SetSettings {
                context: ctx.to_string(),
                plugin: plugin.to_string(),
                settings,
            });
            ok()
        }
        "log" => {
            let level = str_param(p, "level")?;
            let msg = str_param(p, "message")?;
            match level {
                "debug" => tracing::debug!(target: "plugin", plugin, "{msg}"),
                "info" => tracing::info!(target: "plugin", plugin, "{msg}"),
                "warn" => tracing::warn!(target: "plugin", plugin, "{msg}"),
                "error" => tracing::error!(target: "plugin", plugin, "{msg}"),
                _ => return Err((ERR_PARAMS, "level must be debug, info, warn or error".into())),
            }
            shared.push_log(plugin, level, msg);
            ok()
        }
        "openUrl" => {
            let url = str_param(p, "url")?;
            let lower = url.to_ascii_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                return Err((ERR_NOT_ALLOWED, "only http and https URLs can be opened".into()));
            }
            let _ = shared.events.send(HostEvent::OpenUrl { url: url.to_string() });
            ok()
        }
        _ => Err((ERR_METHOD, format!("unknown method {method:?}"))),
    }
}

fn check_ctx(st: &crate::state::State, plugin: &str, ctx: &str) -> Result<(), (i64, String)> {
    let known = st.visible.get(ctx).is_some_and(|v| v.inst.plugin == plugin)
        || st.plugins.get(plugin).is_some_and(|e| e.seen.contains(ctx));
    if known {
        Ok(())
    } else {
        Err((ERR_CONTEXT, format!("unknown context {ctx:?}")))
    }
}

fn check_context(shared: &Shared, plugin: &str, ctx: &str) -> Result<(), (i64, String)> {
    check_ctx(&shared.lock(), plugin, ctx)
}

fn update_look(shared: &Shared, plugin: &str, ctx: &str, f: impl FnOnce(&mut Look)) -> Result<(), (i64, String)> {
    {
        let mut st = shared.lock();
        check_ctx(&st, plugin, ctx)?;
        let look = st.looks.entry(ctx.to_string()).or_default();
        if look.plugin != plugin {
            *look = Look { plugin: plugin.to_string(), ..Default::default() };
        }
        f(look);
    }
    (shared.poke)();
    Ok(())
}

/// Load, render and import an image; the result is cached by the hash of the input.
fn import_image(shared: &Shared, plugin: &str, img: &str) -> Result<String, (i64, String)> {
    let dir =
        shared.lock().plugins.get(plugin).map(|e| e.dir.clone()).ok_or((ERR_PARAMS, "plugin is gone".to_string()))?;
    let map_err = |e: ImageError| match e {
        ImageError::Params(m) => (ERR_PARAMS, m),
        ImageError::NotAllowed(m) => (ERR_NOT_ALLOWED, m),
    };
    let loaded = image::load(&dir, img).map_err(map_err)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    loaded.kind.hash(&mut h);
    loaded.bytes.hash(&mut h);
    let key = h.finish();
    if let Some(hash) = shared.lock().images.get(&key) {
        return Ok(hash.clone());
    }
    let png = image::to_png(&loaded).map_err(map_err)?;
    let hash = (shared.icons)(&png).ok_or((ERR_PARAMS, "the agent could not import the image".to_string()))?;
    shared.lock().images.insert(key, hash.clone());
    Ok(hash)
}

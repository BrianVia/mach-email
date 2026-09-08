use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::WebviewWindow;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use uuid::Uuid;

type Pending = Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>>;
type JsonResponse = Response<Cursor<Vec<u8>>>;

const MAX_BODY: u64 = 1_000_000;
const MAX_DOM_BYTES: usize = 100 * 1024;

#[derive(Deserialize)]
struct EvalRequest {
    js: String,
}

#[derive(Deserialize)]
struct KeyRequest {
    key: String,
    #[serde(default)]
    ctrl: bool,
    #[serde(default)]
    shift: bool,
    #[serde(default)]
    alt: bool,
    #[serde(default)]
    meta: bool,
}

pub fn start(window: WebviewWindow) {
    let Ok(port) = std::env::var("MACH_DEBUG_PORT") else {
        return;
    };
    let Ok(port) = port.parse::<u16>() else {
        tracing::warn!(port, "invalid MACH_DEBUG_PORT");
        return;
    };
    let address = format!("127.0.0.1:{port}");
    let server = match Server::http(&address) {
        Ok(server) => server,
        Err(error) => {
            tracing::warn!(%error, %address, "starting debug sidecar failed");
            return;
        }
    };
    tracing::info!("debug sidecar on {address}");

    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let window = window.clone();
            let pending = pending.clone();
            let address = address.clone();
            std::thread::spawn(move || handle(request, &window, &pending, &address));
        }
    });
}

fn handle(mut request: Request, window: &WebviewWindow, pending: &Pending, address: &str) {
    let method = request.method().clone();
    let path = request.url().to_string();
    let response = match (method, path.as_str()) {
        (Method::Get, "/health") => json_response(StatusCode(200), json!({"ok": true})),
        (Method::Post, "/eval") => match read_json::<EvalRequest>(&mut request) {
            Ok(body) => evaluate(window, pending, address, &body.js, None),
            Err(error) => json_response(StatusCode(400), json!({"err": error})),
        },
        (Method::Post, "/key") => match read_json::<KeyRequest>(&mut request) {
            Ok(key) => {
                let options = json!({
                    "key": key.key,
                    "ctrlKey": key.ctrl,
                    "shiftKey": key.shift,
                    "altKey": key.alt,
                    "metaKey": key.meta,
                    "bubbles": true,
                    "cancelable": true,
                });
                evaluate(
                    window,
                    pending,
                    address,
                    &format!(
                        "(() => {{ (document.activeElement ?? window).dispatchEvent(new KeyboardEvent('keydown', {options})); return true; }})()"
                    ),
                    None,
                )
            }
            Err(error) => json_response(StatusCode(400), json!({"err": error})),
        },
        (Method::Get, "/state") => evaluate(
            window,
            pending,
            address,
            "window.__machDebugState?.() ?? null",
            None,
        ),
        (Method::Get, "/dom") => evaluate(
            window,
            pending,
            address,
            "document.body.innerText",
            Some(MAX_DOM_BYTES),
        ),
        (Method::Post, path) if path.starts_with("/__result/") => {
            let id = &path[10..];
            match read_json::<Value>(&mut request) {
                Ok(value) => match pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(id)
                {
                    Some(sender) => {
                        let _ = sender.send(value);
                        json_response(StatusCode(200), json!({"ok": true}))
                    }
                    None => json_response(StatusCode(404), json!({"err": "unknown result id"})),
                },
                Err(error) => json_response(StatusCode(400), json!({"err": error})),
            }
        }
        _ => json_response(StatusCode(404), json!({"err": "not found"})),
    };
    let _ = request.respond(response);
}

fn evaluate(
    window: &WebviewWindow,
    pending: &Pending,
    address: &str,
    expression: &str,
    max_string_bytes: Option<usize>,
) -> JsonResponse {
    let id = Uuid::new_v4().to_string();
    let callback = serde_json::to_string(&format!("http://{address}/__result/{id}")).unwrap();
    let (sender, receiver) = mpsc::channel();
    pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone(), sender);

    let script = format!(
        "void (async () => {{ try {{ const value = await (async () => ({expression}))(); await fetch({callback}, {{ method: 'POST', body: JSON.stringify({{ ok: value === undefined ? null : value }}) }}); }} catch (error) {{ await fetch({callback}, {{ method: 'POST', body: JSON.stringify({{ err: String(error?.stack ?? error) }}) }}); }} }})()"
    );
    if let Err(error) = window.eval(script) {
        pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
        return json_response(StatusCode(500), json!({"err": error.to_string()}));
    }

    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(mut value) => {
            if let (Some(max), Some(Value::String(text))) = (max_string_bytes, value.get_mut("ok"))
            {
                truncate_utf8(text, max);
            }
            json_response(StatusCode(200), value)
        }
        Err(error) => {
            pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            json_response(StatusCode(504), json!({"err": error.to_string()}))
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T, String> {
    let mut body = String::new();
    request
        .as_reader()
        .take(MAX_BODY)
        .read_to_string(&mut body)
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

fn json_response(status: StatusCode, value: Value) -> JsonResponse {
    let mut response = Response::from_string(value.to_string()).with_status_code(status);
    response.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    response.add_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap());
    response
}

fn truncate_utf8(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::truncate_utf8;

    #[test]
    fn dom_cap_preserves_utf8() {
        let mut text = "é".repeat(60_000);
        truncate_utf8(&mut text, 100 * 1024);
        assert!(text.len() <= 100 * 1024);
        assert!(text.ends_with('é'));
    }
}

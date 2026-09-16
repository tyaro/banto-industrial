//! A scriptable, dependency-free stand-in for banto-hub.
//!
//! Same approach as `banto_tagclient::rest`'s own tests - a real
//! `TcpListener` on `127.0.0.1:0` driven from a background thread - rather
//! than an HTTP mocking crate, so the crate's dependency tree stays exactly
//! the production one. The difference is that a bootstrap run makes
//! *several* requests to the same origin (commissioning status, issue,
//! revoke, catalog), so this harness serves a whole session: it accepts
//! connections in a loop, answers each request from a route table, and
//! records what it was asked.
//!
//! Every response carries `Connection: close`, so reqwest opens a fresh
//! connection per request and the loop never has to multiplex.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// One request the harness saw.
#[derive(Clone, Debug)]
pub struct SeenRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub banto_client: Option<String>,
    pub body: String,
}

impl SeenRequest {
    pub fn route(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

/// A canned `(status, body)` answer.
pub type Reply = (u16, String);

/// A scripted Hub. Routes are keyed by `"METHOD /path"`; each route holds a
/// queue of replies consumed in order, and the last reply repeats once the
/// queue runs dry (so a test only has to script the turns it cares about).
pub struct MockHub {
    address: String,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    stop: Arc<AtomicBool>,
}

impl MockHub {
    pub fn start(routes: HashMap<String, Vec<Reply>>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock hub");
        let address = format!("http://{}", listener.local_addr().expect("local_addr"));
        let seen: Arc<Mutex<Vec<SeenRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_seen = Arc::clone(&seen);
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut routes = routes;
            let mut consumed: HashMap<String, usize> = HashMap::new();
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { return };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let route = request.route();
                thread_seen
                    .lock()
                    .expect("mock hub log poisoned")
                    .push(request);

                let (status, body) = match routes.get_mut(&route) {
                    Some(replies) if !replies.is_empty() => {
                        let index = consumed.entry(route.clone()).or_insert(0);
                        let reply = replies[(*index).min(replies.len() - 1)].clone();
                        *index += 1;
                        reply
                    }
                    _ => (404, String::from("{}")),
                };
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            address,
            seen,
            stop,
        }
    }

    /// The `http://127.0.0.1:{port}` base URL to hand to the bootstrapper.
    pub fn endpoint(&self) -> String {
        self.address.clone()
    }

    pub fn seen(&self) -> Vec<SeenRequest> {
        self.seen.lock().expect("mock hub log poisoned").clone()
    }

    pub fn routes_seen(&self) -> Vec<String> {
        self.seen().iter().map(SeenRequest::route).collect()
    }

    pub fn hit_count(&self, route: &str) -> usize {
        self.routes_seen()
            .iter()
            .filter(|seen| *seen == route)
            .count()
    }
}

impl Drop for MockHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<SeenRequest> {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 1024];
    let header_end = loop {
        let count = stream.read(&mut buffer).ok()?;
        if count == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..count]);
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };

    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let path = target
        .split('?')
        .next()
        .unwrap_or(target.as_str())
        .to_owned();

    let mut authorization = None;
    let mut banto_client = None;
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        match name.to_ascii_lowercase().as_str() {
            "authorization" => authorization = Some(value),
            "x-banto-client" => banto_client = Some(value),
            "content-length" => content_length = value.parse().unwrap_or(0),
            _ => {}
        }
    }

    let mut body_bytes = raw[header_end..].to_vec();
    while body_bytes.len() < content_length {
        let count = stream.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        body_bytes.extend_from_slice(&buffer[..count]);
    }

    Some(SeenRequest {
        method,
        path,
        authorization,
        banto_client,
        body: String::from_utf8_lossy(&body_bytes).to_string(),
    })
}

/// A `GET /api/v1/tags` body with `count` tags - `0` is the acceptance
/// criteria's "接続済み・利用可能なタグなし" case.
pub fn catalog_body(count: usize) -> String {
    let tags: Vec<String> = (0..count)
        .map(|index| {
            format!(
                r#"{{"external_name":"line1.fast.tag{index}","tag_key":"line1.fast.tag{index}",
                    "ids":[1,1,{index}],"connection":"line1","group":"fast","name":"tag{index}",
                    "address":"D{index}","data_type":"real","unit":"degC","decimals":1,
                    "period_ms":1000,"enabled":true,"writable":false,"tag_kind":"device",
                    "expression":null,"retain":false,"simulation":false,
                    "configured_simulation":false,"effective_simulation":false,
                    "value_source":"real"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"revision":1,"run_id":2,"collection_mode":"configured","tags":[{}]}}"#,
        tags.join(",")
    )
}

/// `GET /api/commissioning/status`'s body.
pub fn commissioning_body(locked_down: bool) -> String {
    format!(r#"{{"lockedDown":{locked_down}}}"#)
}

/// `POST /api/api-keys`'s success body.
pub fn issued_body(id: i64, name: &str, key: &str) -> String {
    format!(r#"{{"id":{id},"name":"{name}","prefix":"abcd1234","scopes":["read"],"key":"{key}"}}"#)
}

/// banto-hub's duplicate-name rejection (a `validation` error on `name`,
/// not a 409).
pub fn duplicate_name_body() -> String {
    r#"{"kind":"validation","field_errors":[{"field":"name","message":"この名前は既に使用されています"}]}"#
        .to_owned()
}

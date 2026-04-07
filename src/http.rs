use crate::graph::NodeId;
use std::collections::HashMap;
use std::sync::mpsc;

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub enum HttpAction {
    SendRequest {
        node_id: NodeId,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: String,
    },
}

struct PendingRequest {
    rx: mpsc::Receiver<HttpResponse>,
}

pub struct HttpManager {
    pending: HashMap<NodeId, PendingRequest>,
}

impl HttpManager {
    pub fn new() -> Self {
        Self { pending: HashMap::new() }
    }

    pub fn process(&mut self, actions: Vec<HttpAction>) {
        for action in actions {
            match action {
                HttpAction::SendRequest { node_id, url, method, headers, body } => {
                    let (tx, rx) = mpsc::channel();
                    self.pending.insert(node_id, PendingRequest { rx });

                    // Log the outgoing request to the system console so the
                    // user can see (and debug) every HTTP / AI call. We
                    // truncate URL and body to keep the line readable, and
                    // never log raw API key headers.
                    let method_up = method.to_uppercase();
                    let display_url = redact_url_query(&url);
                    let body_len = body.len();
                    crate::system_log::log(format!(
                        "HTTP → node#{} {} {} ({} body bytes)",
                        node_id, method_up, truncate(&display_url, 120), body_len
                    ));

                    let log_method = method_up.clone();
                    let log_url = display_url.clone();
                    let started = std::time::Instant::now();
                    std::thread::spawn(move || {
                        let client = reqwest::blocking::Client::builder()
                            .timeout(std::time::Duration::from_secs(60))
                            .build()
                            .unwrap_or_else(|_| reqwest::blocking::Client::new());

                        let mut req = if method.to_uppercase() == "GET" {
                            client.get(&url)
                        } else {
                            client.post(&url)
                        };

                        for (key, val) in &headers {
                            req = req.header(key.as_str(), val.as_str());
                        }

                        if method.to_uppercase() != "GET" && !body.is_empty() {
                            req = req.body(body);
                        }

                        let resp = match req.send() {
                            Ok(r) => {
                                let status = r.status().as_u16();
                                let body = r.text().unwrap_or_default();
                                HttpResponse { status, body }
                            }
                            Err(e) => HttpResponse {
                                status: 0,
                                body: format!("Error: {}", e),
                            },
                        };

                        // Log the response. Errors and non-2xx statuses are
                        // logged at warn/error level with a body snippet so
                        // it's easy to spot bad keys, 401s, rate limits etc.
                        let elapsed_ms = started.elapsed().as_millis();
                        let summary = format!(
                            "HTTP ← node#{} {} {} → {} in {}ms ({} bytes)",
                            node_id, log_method, truncate(&log_url, 80),
                            resp.status, elapsed_ms, resp.body.len()
                        );
                        if resp.status == 0 {
                            crate::system_log::error(format!(
                                "{} | {}", summary, truncate(&resp.body, 240)
                            ));
                        } else if !(200..300).contains(&resp.status) {
                            crate::system_log::warn(format!(
                                "{} | {}", summary, truncate(&resp.body, 240)
                            ));
                        } else {
                            crate::system_log::log(summary);
                        }

                        let _ = tx.send(resp);
                    });
                }
            }
        }
    }

    pub fn poll(&mut self, node_id: NodeId) -> Option<HttpResponse> {
        if let Some(pending) = self.pending.get(&node_id) {
            match pending.rx.try_recv() {
                Ok(resp) => {
                    self.pending.remove(&node_id);
                    Some(resp)
                }
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending.remove(&node_id);
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn is_pending(&self, node_id: NodeId) -> bool {
        self.pending.contains_key(&node_id)
    }
}

/// Cap a string to `max` characters with an ellipsis suffix.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{}…", head)
    }
}

/// Redact obvious secret-looking query params (e.g. `?key=...`,
/// `?api_key=...`, `?token=...`) so the system console doesn't leak
/// API keys when logging Google / Imagen URLs.
fn redact_url_query(url: &str) -> String {
    let Some(q_pos) = url.find('?') else { return url.to_string(); };
    let (base, query) = url.split_at(q_pos);
    let query = &query[1..];
    let cleaned: Vec<String> = query.split('&').map(|kv| {
        if let Some(eq) = kv.find('=') {
            let (k, _) = kv.split_at(eq);
            let lower = k.to_ascii_lowercase();
            if lower.contains("key") || lower.contains("token") || lower.contains("secret") {
                return format!("{}=***", k);
            }
        }
        kv.to_string()
    }).collect();
    format!("{}?{}", base, cleaned.join("&"))
}

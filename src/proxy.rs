use crate::json_usage::JsonUsageExtractor;
use crate::parsers::{model_from_request_body, model_from_response_value};
use crate::paths::calls_db;
use crate::pricing;
use crate::providers::{MergeSse, ParseJson, Provider, PROVIDERS};
use crate::record::{classify_error, Record, Store, Usage};
use crate::sse::SseSplitter;
use anyhow::Result;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use bytes::Bytes;
use futures_util::{stream, Stream, StreamExt};
use jiff::Timestamp;
use log::{info, warn};
use reqwest::{Body as ReqwestBody, Client};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, error::TrySendError};

#[derive(Clone)]
struct ProxyState {
    provider: &'static Provider,
    client: Client,
    store: Arc<Mutex<Store>>,
}

const MAX_MODEL_INSPECT_BYTES: usize = 256 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 64 * 1024;
const BODY_CHANNEL_CAP: usize = 4;
const OBSERVER_CHANNEL_CAP: usize = 4;

#[derive(Clone)]
struct RecordBase {
    id: String,
    ts: String,
    provider: String,
    model: Option<String>,
    endpoint: String,
    status: Option<u16>,
    stream: bool,
    started: Instant,
}

enum ObserveMsg {
    Chunk {
        bytes: Bytes,
        elapsed_ms: u64,
    },
    Finish {
        elapsed_ms: u64,
    },
    UpstreamError {
        elapsed_ms: u64,
        kind: String,
        message: String,
    },
    ClientDisconnect {
        elapsed_ms: u64,
    },
}

enum ObserverKind {
    Sse {
        merge: MergeSse,
    },
    Json {
        parse: ParseJson,
        usage_key: &'static str,
        enabled: bool,
    },
}

pub async fn run_all() -> Result<()> {
    crate::pricing::init(&crate::paths::prices_json());
    let client = Client::builder().use_rustls_tls().build()?;
    let store = Arc::new(Mutex::new(Store::open(&calls_db())?));

    let mut handles = Vec::new();

    for provider in PROVIDERS {
        let state = ProxyState {
            provider,
            client: client.clone(),
            store: store.clone(),
        };

        let app = Router::new()
            .fallback(handle_request)
            .with_state(Arc::new(state));

        let addr = SocketAddr::from(([127, 0, 0, 1], provider.default_port));
        let listener = TcpListener::bind(addr).await?;
        info!("toll [{}] listening on http://{}", provider.name, addr);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .unwrap_or_else(|e| warn!("serve error: {e}"));
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.await;
    }

    // All servers have stopped; fold the WAL back and refresh stats so the
    // DB is compact and query-ready at rest.
    store.lock().unwrap_or_else(|e| e.into_inner()).checkpoint();
    Ok(())
}

async fn handle_request(
    State(state): State<Arc<ProxyState>>,
    req: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    let provider = state.provider;
    let t0 = Instant::now();
    let ts = Timestamp::now().to_string();
    let call_id = new_call_id();

    let (parts, body) = req.into_parts();
    let method = parts.method;
    let uri = parts.uri;
    let path = uri.path();
    let headers = parts.headers;

    // Model from path (Gemini) or from body.
    let model_from_path = provider.model_from_path.and_then(|f| f(path));

    let needs_body_read = should_inspect_body(&headers)
        && (model_from_path.is_none() || provider.inject_stream_options);
    let (model_from_body, upstream_body) = if needs_body_read {
        let body_bytes = axum::body::to_bytes(body, MAX_MODEL_INSPECT_BYTES)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let model = if model_from_path.is_none() {
            model_from_request_body(&body_bytes)
        } else {
            None
        };
        let forwarded = if provider.inject_stream_options {
            maybe_inject_stream_options(body_bytes)
        } else {
            body_bytes
        };
        (model, ReqwestBody::from(forwarded))
    } else {
        (None, ReqwestBody::wrap_stream(body.into_data_stream()))
    };

    let model = model_from_path.or(model_from_body);

    // Build upstream URL.
    let upstream = format!("{}{}", provider.upstream_url, uri);

    let mut upstream_req = state
        .client
        .request(method.clone(), &upstream)
        .body(upstream_body);

    // Forward end-to-end headers. reqwest derives Host from the upstream URL
    // and Content-Length from the body, so we strip both here.
    let request_connection_tokens = connection_tokens(&headers);
    for (name, value) in &headers {
        if *name == header::HOST
            || *name == header::ACCEPT_ENCODING
            || *name == header::CONTENT_LENGTH
            || is_hop_by_hop_header(name, &request_connection_tokens)
        {
            continue;
        }
        upstream_req = upstream_req.header(name, value);
    }

    let endpoint = path.split('?').next().unwrap_or(path).to_string();

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            let message = sanitized_reqwest_error(&e);
            let kind = classify_error(&message);
            if is_inference_endpoint(&endpoint) {
                let rec = Record {
                    id: call_id,
                    ts,
                    provider: provider.name.to_string(),
                    model,
                    status: None,
                    latency_ms: t0.elapsed().as_millis() as u64,
                    ttft_ms: None,
                    stream: false,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                    reasoning_output_tokens: None,
                    error_kind: Some(kind.to_string()),
                    error_message: Some(message),
                    cost: None,
                };
                spawn_record_write(state.store.clone(), rec);
            }
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();

    let is_sse = resp_headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    // Build response builder with upstream status + headers.
    let mut builder = Response::builder().status(status.as_u16());
    let response_connection_tokens = connection_tokens(&resp_headers);
    for (name, value) in &resp_headers {
        if is_hop_by_hop_header(name, &response_connection_tokens) {
            continue;
        }
        builder = builder.header(name, value);
    }

    let base = RecordBase {
        id: call_id,
        ts,
        provider: provider.name.to_string(),
        model,
        endpoint,
        status: Some(status.as_u16()),
        stream: is_sse,
        started: t0,
    };

    let observer_kind = if is_sse {
        ObserverKind::Sse {
            merge: provider.merge_sse,
        }
    } else {
        ObserverKind::Json {
            parse: provider.parse_json,
            usage_key: provider.json_usage_key,
            enabled: status.is_success() && is_json_response(&resp_headers),
        }
    };

    let (body_tx, body_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(BODY_CHANNEL_CAP);
    let (obs_tx, obs_rx) = mpsc::channel::<ObserveMsg>(OBSERVER_CHANNEL_CAP);
    let observer_dropped = Arc::new(AtomicBool::new(false));

    spawn_observer(
        observer_kind,
        base.clone(),
        state.store.clone(),
        observer_dropped.clone(),
        obs_rx,
    );

    let mut byte_stream = upstream_resp.bytes_stream();
    let forward_task = tokio::spawn(async move {
        while let Some(chunk_res) = byte_stream.next().await {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => {
                    let message = sanitized_reqwest_error(&e);
                    let kind = classify_error(&message).to_string();
                    let _ = body_tx
                        .send(Err(std::io::Error::other(message.clone())))
                        .await;
                    drop(body_tx);
                    let _ = obs_tx
                        .send(ObserveMsg::UpstreamError {
                            elapsed_ms: base.started.elapsed().as_millis() as u64,
                            kind,
                            message,
                        })
                        .await;
                    return;
                }
            };

            if !observer_dropped.load(Ordering::Relaxed) {
                try_observe(
                    &obs_tx,
                    &observer_dropped,
                    ObserveMsg::Chunk {
                        bytes: chunk.clone(),
                        elapsed_ms: base.started.elapsed().as_millis() as u64,
                    },
                );
            }

            if body_tx.send(Ok(chunk)).await.is_err() {
                drop(body_tx);
                let _ = obs_tx
                    .send(ObserveMsg::ClientDisconnect {
                        elapsed_ms: base.started.elapsed().as_millis() as u64,
                    })
                    .await;
                return;
            }
        }

        drop(body_tx);
        let _ = obs_tx
            .send(ObserveMsg::Finish {
                elapsed_ms: base.started.elapsed().as_millis() as u64,
            })
            .await;
    });
    std::mem::drop(forward_task);

    let body = Body::from_stream(receiver_stream(body_rx));
    builder
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn new_call_id() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).unwrap_or(());
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn spawn_observer(
    kind: ObserverKind,
    mut base: RecordBase,
    store: Arc<Mutex<Store>>,
    dropped: Arc<AtomicBool>,
    mut rx: mpsc::Receiver<ObserveMsg>,
) {
    let handle = tokio::spawn(async move {
        let mut usage = Usage::default();
        let mut ttft_ms: Option<u64> = None;
        let mut sse_splitter = match &kind {
            ObserverKind::Sse { .. } => Some(SseSplitter::new(MAX_SSE_EVENT_BYTES)),
            ObserverKind::Json { .. } => None,
        };
        let mut json_extractor = match &kind {
            ObserverKind::Json { usage_key, .. } => Some(JsonUsageExtractor::new(usage_key)),
            ObserverKind::Sse { .. } => None,
        };

        while let Some(msg) = rx.recv().await {
            match msg {
                ObserveMsg::Chunk { bytes, elapsed_ms } => {
                    if dropped.load(Ordering::Relaxed) {
                        continue;
                    }

                    match &kind {
                        ObserverKind::Sse { merge } => {
                            if ttft_ms.is_none() && !bytes.is_empty() {
                                ttft_ms = Some(elapsed_ms);
                            }
                            let Some(splitter) = sse_splitter.as_mut() else {
                                continue;
                            };
                            let events = match splitter.push(&bytes) {
                                Ok(events) => events,
                                Err(_) => {
                                    dropped.store(true, Ordering::Relaxed);
                                    continue;
                                }
                            };
                            for event in events {
                                if !should_parse_sse_event(&event.event_type, &event.data) {
                                    continue;
                                }
                                if let Ok(data) = serde_json::from_str::<Value>(&event.data) {
                                    if data.is_object() {
                                        // Backfill model when the request body
                                        // was too large to inspect; streaming
                                        // responses echo it on the chunk that
                                        // also carries usage / message_start.
                                        if base.model.is_none() {
                                            base.model = model_from_response_value(&data);
                                        }
                                        merge(&event.event_type, &data, &mut usage);
                                    }
                                }
                            }
                        }
                        ObserverKind::Json { enabled, .. } => {
                            if *enabled {
                                if let Some(extractor) = json_extractor.as_mut() {
                                    extractor.push(&bytes);
                                }
                            }
                        }
                    }
                }
                ObserveMsg::Finish { elapsed_ms } => {
                    if dropped.load(Ordering::Relaxed) {
                        usage = Usage::default();
                    } else if let ObserverKind::Json { parse, enabled, .. } = kind {
                        if enabled {
                            if let Some(extractor) = json_extractor.take() {
                                if base.model.is_none() {
                                    base.model = extractor.model().map(String::from);
                                }
                                usage = extractor
                                    .finish_wrapped()
                                    .map(|v| parse(&v))
                                    .unwrap_or_default();
                            }
                        }
                    }
                    if is_inference_endpoint(&base.endpoint) {
                        spawn_record_write(
                            store,
                            record_from_base(&base, usage, elapsed_ms, ttft_ms, None, None),
                        );
                    }
                    return;
                }
                ObserveMsg::UpstreamError {
                    elapsed_ms,
                    kind,
                    message,
                } => {
                    if dropped.load(Ordering::Relaxed) {
                        usage = Usage::default();
                    }
                    if is_inference_endpoint(&base.endpoint) {
                        spawn_record_write(
                            store,
                            record_from_base(
                                &base,
                                usage,
                                elapsed_ms,
                                ttft_ms,
                                Some(kind),
                                Some(message),
                            ),
                        );
                    }
                    return;
                }
                ObserveMsg::ClientDisconnect { elapsed_ms } => {
                    if dropped.load(Ordering::Relaxed) {
                        usage = Usage::default();
                    }
                    if is_inference_endpoint(&base.endpoint) {
                        spawn_record_write(
                            store,
                            record_from_base(
                                &base,
                                usage,
                                elapsed_ms,
                                ttft_ms,
                                Some("client_disconnect".to_string()),
                                Some("downstream client disconnected".to_string()),
                            ),
                        );
                    }
                    return;
                }
            }
        }
    });
    std::mem::drop(handle);
}

fn try_observe(tx: &mpsc::Sender<ObserveMsg>, dropped: &AtomicBool, msg: ObserveMsg) {
    match tx.try_send(msg) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => dropped.store(true, Ordering::Relaxed),
        Err(TrySendError::Closed(_)) => {}
    }
}

fn record_from_base(
    base: &RecordBase,
    usage: Usage,
    latency_ms: u64,
    ttft_ms: Option<u64>,
    error_kind: Option<String>,
    error_message: Option<String>,
) -> Record {
    let cost = pricing::compute_cost(base.model.as_deref(), &usage);
    Record {
        id: base.id.clone(),
        ts: base.ts.clone(),
        provider: base.provider.clone(),
        model: base.model.clone(),
        status: base.status,
        latency_ms,
        ttft_ms,
        stream: base.stream,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        error_kind,
        error_message,
        cost,
    }
}

/// toll records *inference* — requests that consume tokens and cost money.
/// The OpenAI-compatible inference surface (plus Anthropic / Gemini) is
/// small and stable; the junk clients probe (`/api/tags`, `/version`,
/// `/props`, model listings, ...) is open-ended. So we allowlist inference
/// rather than chase an ever-growing denylist of probes. Calls are still
/// proxied normally — this only governs whether we log them.
///
/// Bias toward inclusion: anything that can carry a `usage` object must
/// match, or we silently lose cost data.
fn is_inference_endpoint(endpoint: &str) -> bool {
    const MARKERS: &[&str] = &[
        "/completions", // /v1/completions and /v1/chat/completions
        "/embeddings",
        "/messages",       // Anthropic
        "/responses",      // OpenAI Responses API
        "generatecontent", // Gemini :generateContent / :streamGenerateContent
    ];
    let e = endpoint.to_ascii_lowercase();
    MARKERS.iter().any(|m| e.contains(m))
}

fn spawn_record_write(store: Arc<Mutex<Store>>, record: Record) {
    let handle = tokio::task::spawn_blocking(move || {
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = s.insert(&record) {
            warn!("failed to write toll record: {e}");
        }
    });
    std::mem::drop(handle);
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).unwrap_or_else(|_| {
        // Fall back to ctrl_c only if SIGTERM setup fails (shouldn't happen).
        panic!("failed to install SIGTERM handler")
    });
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

fn should_inspect_body(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len <= MAX_MODEL_INSPECT_BYTES)
}

fn is_json_response(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"))
}

fn receiver_stream(
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    })
}

fn should_parse_sse_event(event_type: &str, data: &str) -> bool {
    matches!(event_type, "message_start" | "message_delta")
        || data.contains("\"usage\"")
        || data.contains("\"usageMetadata\"")
}

fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_hop_by_hop_header(name: &HeaderName, connection_tokens: &[String]) -> bool {
    let name = name.as_str();
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    ) || connection_tokens
        .iter()
        .any(|token| token.eq_ignore_ascii_case(name))
}

fn sanitized_reqwest_error(err: &reqwest::Error) -> String {
    sanitize_error_message(&err.to_string(), err.url())
}

fn sanitize_error_message(message: &str, url: Option<&reqwest::Url>) -> String {
    let Some(url) = url else {
        return message.to_string();
    };
    message.replace(url.as_str(), &redacted_url(url))
}

fn redacted_url(url: &reqwest::Url) -> String {
    let mut redacted = url.clone();
    let _ = redacted.set_username("");
    let _ = redacted.set_password(None);
    redacted.set_query(None);
    redacted.set_fragment(None);
    redacted.to_string()
}

/// If the body is a streaming JSON request (`"stream": true`), inject
/// `stream_options: {"include_usage": true}` so the final SSE chunk carries
/// token counts. Falls back to the original bytes on any parse failure.
fn maybe_inject_stream_options(body: Bytes) -> Bytes {
    let Ok(mut v) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(obj) = v.as_object_mut() else {
        return body;
    };
    if obj.get("stream").and_then(|v| v.as_bool()) != Some(true) {
        return body;
    }
    match obj.get_mut("stream_options") {
        Some(Value::Object(so)) => {
            so.insert("include_usage".to_string(), serde_json::json!(true));
        }
        Some(_) => {
            obj.insert(
                "stream_options".to_string(),
                serde_json::json!({"include_usage": true}),
            );
        }
        None => {
            obj.insert(
                "stream_options".to_string(),
                serde_json::json!({"include_usage": true}),
            );
        }
    }
    serde_json::to_vec(&v).map(Bytes::from).unwrap_or(body)
}

/// Spawn a proxy on a caller-supplied listener (for tests).
#[allow(dead_code)]
pub async fn serve_on(
    listener: TcpListener,
    provider: &'static Provider,
    store: Arc<Mutex<Store>>,
) -> tokio::task::JoinHandle<()> {
    let client = Client::builder().use_rustls_tls().build().unwrap();
    let state = Arc::new(ProxyState {
        provider,
        client,
        store,
    });
    let app = Router::new().fallback(handle_request).with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bytes(v: serde_json::Value) -> Bytes {
        Bytes::from(serde_json::to_vec(&v).unwrap())
    }

    #[test]
    fn inject_adds_stream_options_when_streaming() {
        let out = maybe_inject_stream_options(bytes(json!({"model": "gpt-4o", "stream": true})));
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn inject_skips_non_streaming_requests() {
        let input = bytes(json!({"model": "gpt-4o", "stream": false}));
        let out = maybe_inject_stream_options(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn inject_skips_requests_without_stream_field() {
        let input = bytes(json!({"model": "gpt-4o"}));
        let out = maybe_inject_stream_options(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn inject_overwrites_existing_include_usage() {
        let out = maybe_inject_stream_options(bytes(json!({
            "model": "gpt-4o",
            "stream": true,
            "stream_options": {"include_usage": false, "extra": 1}
        })));
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], json!(true));
        assert_eq!(v["stream_options"]["extra"], json!(1));
    }

    #[test]
    fn inject_replaces_invalid_stream_options() {
        let out = maybe_inject_stream_options(bytes(json!({
            "model": "gpt-4o",
            "stream": true,
            "stream_options": false
        })));
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn inject_falls_back_on_invalid_json() {
        let garbage = Bytes::from_static(b"not json");
        let out = maybe_inject_stream_options(garbage.clone());
        assert_eq!(out, garbage);
    }

    #[test]
    fn inference_endpoints_are_recorded() {
        for e in [
            "/v1/chat/completions",
            "/api/v1/chat/completions", // OpenRouter
            "/v1/completions",
            "/v1/embeddings",
            "/v1/messages",                              // Anthropic
            "/v1/responses",                             // OpenAI Responses
            "/v1beta/models/gemini-2.0:generateContent", // Gemini
            "/v1beta/models/gemini-2.0:streamGenerateContent",
        ] {
            assert!(is_inference_endpoint(e), "{e} should be recorded");
        }
    }

    #[test]
    fn probes_and_listings_are_skipped() {
        for e in [
            "/api/tags",
            "/api/show",
            "/api/v1/models",
            "/props",
            "/v1/props",
            "/version",
            "/v1/models",
            "/v1/models/deepseek-v4-pro",
        ] {
            assert!(!is_inference_endpoint(e), "{e} should be skipped");
        }
    }

    #[test]
    fn error_url_is_redacted_before_persistence() {
        let url = reqwest::Url::parse(
            "https://user:secret@api.example.com/v1/messages?api_key=sk-secret&alt=sse#frag",
        )
        .unwrap();
        let message = format!("error sending request for url ({url})");
        let sanitized = sanitize_error_message(&message, Some(&url));

        assert!(!sanitized.contains("sk-secret"));
        assert!(!sanitized.contains("user:secret"));
        assert!(!sanitized.contains("alt=sse"));
        assert_eq!(
            sanitized,
            "error sending request for url (https://api.example.com/v1/messages)"
        );
    }
}

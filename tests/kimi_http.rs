use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use grok_build_proxy::{
    auth::{CredentialProvider, Credentials},
    catalog::Catalog,
    modelmap::ModelMap,
    provider::kimi::auth::Store as KimiStore,
    proxy::{CompatMode, DEFAULT_CODEX_COMPATIBILITY_VERSION, KimiConfig, ProxyConfig, router},
};
use serde_json::Value;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

struct CodexCredentials;

#[async_trait::async_trait]
impl CredentialProvider for CodexCredentials {
    async fn get(&self, _: bool) -> Result<Credentials> {
        Ok(Credentials {
            access_token: "codex-token".into(),
            account_id: String::new(),
            expires_at: None,
        })
    }
}

struct SlowUnavailableCodexCredentials;

#[async_trait::async_trait]
impl CredentialProvider for SlowUnavailableCodexCredentials {
    async fn get(&self, _: bool) -> Result<Credentials> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Err(anyhow!("Codex credentials unavailable"))
    }
}

#[tokio::test]
async fn unscoped_readiness_returns_when_kimi_is_ready() {
    let directory = tempfile::tempdir().unwrap();
    let auth_path = directory.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"access":"kimi-token","refresh":"refresh","expires":4102444800000,"userId":"user-1"}"#,
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }

    let app = router(ProxyConfig {
        upstream_url: "http://127.0.0.1:9/responses".into(),
        credentials: Arc::new(SlowUnavailableCodexCredentials),
        kimi: Some(KimiConfig {
            upstream_url: "http://127.0.0.1:9/chat/completions".into(),
            credentials: Arc::new(KimiStore::new(&auth_path, "http://127.0.0.1:9").unwrap()),
            api_key: String::new(),
        }),
        catalog: Catalog::default(),
        model_map: ModelMap::default(),
        client: reqwest::Client::new(),
        client_token: String::new(),
        version: "test".into(),
        compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
        responses_compat: CompatMode::Full,
        lite_tool_batching: false,
        observer: None,
        max_body_bytes: 4096,
    })
    .unwrap();

    let response = tokio::time::timeout(
        Duration::from_millis(250),
        app.oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("ready Kimi credentials must not wait for Codex")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn kimi_k3_uses_api_key_without_cli_identity_headers() {
    let capture = Capture::default();
    let upstream = Router::new()
        .route("/chat/completions", post(kimi_upstream))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let directory = tempfile::tempdir().unwrap();
    let app = router(ProxyConfig {
        upstream_url: "http://127.0.0.1:9/responses".into(),
        credentials: Arc::new(CodexCredentials),
        kimi: Some(KimiConfig {
            upstream_url: format!("http://{address}/chat/completions"),
            credentials: Arc::new(
                KimiStore::new(
                    directory.path().join("missing-auth.json"),
                    "http://127.0.0.1:9",
                )
                .unwrap(),
            ),
            api_key: "kimi-api-key".into(),
        }),
        catalog: Catalog::default(),
        model_map: ModelMap::default(),
        client: reqwest::Client::new(),
        client_token: String::new(),
        version: "test".into(),
        compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
        responses_compat: CompatMode::Full,
        lite_tool_batching: false,
        observer: None,
        max_body_bytes: 4096,
    })
    .unwrap();

    let ready = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/readyz?provider=kimi")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"k3","input":"hello","reasoning":{"effort":"xhigh"},"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["model"], "k3");

    let requests = capture.0.lock().await;
    let (headers, request) = &requests[0];
    assert_eq!(headers[header::AUTHORIZATION], "Bearer kimi-api-key");
    assert_eq!(
        headers[header::USER_AGENT],
        format!("grok-build-proxy/{}", env!("CARGO_PKG_VERSION"))
    );
    assert!(headers.get("x-msh-platform").is_none());
    assert!(headers.get("x-msh-device-id").is_none());
    assert_eq!(request["model"], "k3");
    assert_eq!(request["reasoning_effort"], "max");
}

#[tokio::test]
async fn kimi_model_routes_to_chat_completions_and_translates_stream() {
    let capture = Capture::default();
    let upstream = Router::new()
        .route("/chat/completions", post(kimi_upstream))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let directory = tempfile::tempdir().unwrap();
    let auth_path = directory.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"access":"kimi-token","refresh":"refresh","expires":4102444800000,"userId":"user-1"}"#,
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
    tokio::fs::write(directory.path().join("device_id"), "device-1\n")
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(
            directory.path().join("device_id"),
            std::fs::Permissions::from_mode(0o600),
        )
        .await
        .unwrap();
    }

    let app = router(ProxyConfig {
        upstream_url: "http://127.0.0.1:9/responses".into(),
        credentials: Arc::new(CodexCredentials),
        kimi: Some(KimiConfig {
            upstream_url: format!("http://{address}/chat/completions"),
            credentials: Arc::new(KimiStore::new(&auth_path, "http://127.0.0.1:9").unwrap()),
            api_key: String::new(),
        }),
        catalog: Catalog::default(),
        model_map: ModelMap::default(),
        client: reqwest::Client::new(),
        client_token: String::new(),
        version: "test".into(),
        compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
        responses_compat: CompatMode::Full,
        lite_tool_batching: false,
        observer: None,
        max_body_bytes: 4096,
    })
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let readiness: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(readiness, serde_json::json!({"ok":true,"auth":"ready"}));

    for provider in ["codex", "kimi"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/readyz?provider={provider}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let readiness: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(readiness["provider"], provider);
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"kimi-k2.6","input":42}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let validation: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(validation["error"]["type"], "invalid_request_error");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"kimi-k2.6","input":"hello","reasoning":{"effort":"high"},"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream; charset=utf-8"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("response.reasoning_summary_text.delta"));
    assert!(body.contains("response.output_text.delta"));
    assert!(body.contains("KIMI_HTTP_OK"));
    assert!(body.contains("response.completed"));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"k2.6","input":"hello","stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let response: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(response["status"], "completed");
    assert_eq!(response["output"][1]["content"][0]["text"], "KIMI_HTTP_OK");

    let requests = capture.0.lock().await;
    assert_eq!(requests.len(), 2);
    let (headers, request) = &requests[0];
    assert_eq!(headers[header::AUTHORIZATION], "Bearer kimi-token");
    assert_eq!(headers["x-msh-device-id"], "device-1");
    assert_eq!(request["model"], "kimi-for-coding");
    assert_eq!(request["messages"][0]["role"], "user");
    assert_eq!(request["messages"][0]["content"], "hello");
    assert_eq!(request["reasoning_effort"], "high");
}

async fn kimi_upstream(
    State(capture): State<Capture>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> impl IntoResponse {
    capture.0.lock().await.push((headers, request));
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"KIMI_HTTP_OK\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        ),
    )
}

#[tokio::test]
async fn kimi_non_success_is_mapped_to_responses_error_contract() {
    let capture = Capture::default();
    let upstream = Router::new()
        .route("/chat/completions", post(kimi_error_upstream))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let directory = tempfile::tempdir().unwrap();
    let auth_path = directory.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"access":"kimi-token","refresh":"refresh","expires":4102444800000,"userId":"user-1"}"#,
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
    tokio::fs::write(directory.path().join("device_id"), "device-1\n")
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(
            directory.path().join("device_id"),
            std::fs::Permissions::from_mode(0o600),
        )
        .await
        .unwrap();
    }

    let app = router(ProxyConfig {
        upstream_url: "http://127.0.0.1:9/responses".into(),
        credentials: Arc::new(CodexCredentials),
        kimi: Some(KimiConfig {
            upstream_url: format!("http://{address}/chat/completions"),
            credentials: Arc::new(KimiStore::new(&auth_path, "http://127.0.0.1:9").unwrap()),
            api_key: String::new(),
        }),
        catalog: Catalog::default(),
        model_map: ModelMap::default(),
        client: reqwest::Client::new(),
        client_token: String::new(),
        version: "test".into(),
        compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
        responses_compat: CompatMode::Full,
        lite_tool_batching: false,
        observer: None,
        max_body_bytes: 4096,
    })
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"kimi-k2.6","input":"hello","stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["message"], "too many requests");
    // Must not leak Chat Completions-shaped fields as the top-level payload.
    assert!(body.get("choices").is_none());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"kimi-k2.6","input":"hello","stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["message"], "too many requests");
    assert!(body.get("choices").is_none());
}

async fn kimi_error_upstream(
    State(capture): State<Capture>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> impl IntoResponse {
    capture.0.lock().await.push((headers, request));
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":{"type":"rate_limit_error","message":"too many requests"},"choices":[]}"#,
    )
}

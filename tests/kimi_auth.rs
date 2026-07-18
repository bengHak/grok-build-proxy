use std::{collections::HashMap, sync::Arc};

use axum::{Form, Json, Router, extract::State, http::HeaderMap, routing::post};
use grok_build_proxy::{
    auth::CredentialProvider,
    provider::kimi::auth::{Store, StoredAuth},
};
use serde_json::{Value, json};
use tokio::{fs, sync::Mutex};

#[derive(Clone, Default)]
struct OAuthState {
    forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
    headers: Arc<Mutex<Vec<HeaderMap>>>,
    polls: Arc<Mutex<usize>>,
}

#[tokio::test]
async fn expired_kimi_credentials_refresh_and_rotate_tokens() {
    let state = OAuthState::default();
    let app = Router::new()
        .route("/api/oauth/token", post(refresh))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    fs::write(
        &path,
        serde_json::to_vec(&StoredAuth {
            access: "expired".into(),
            refresh: "refresh-me".into(),
            expires: 1,
            scope: None,
            user_id: Some("user-1".into()),
        })
        .unwrap(),
    )
    .await
    .unwrap();
    fs::write(dir.path().join("device_id"), "stable-device-id\n")
        .await
        .unwrap();

    let store = Store::new(&path, format!("http://{address}")).unwrap();
    let credentials = store.get(false).await.unwrap();
    assert_eq!(credentials.access_token, "fresh-access");
    assert_eq!(credentials.account_id, "user-1");

    let saved: StoredAuth = serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
    assert_eq!(saved.refresh, "rotated-refresh");
    assert!(saved.expires > 1);
    let forms = state.forms.lock().await;
    assert_eq!(forms[0]["grant_type"], "refresh_token");
    assert_eq!(forms[0]["refresh_token"], "refresh-me");
    let headers = state.headers.lock().await;
    assert_eq!(headers[0]["x-msh-device-id"], "stable-device-id");
}

#[tokio::test]
async fn kimi_device_login_polls_and_persists_credentials() {
    let state = OAuthState::default();
    let app = Router::new()
        .route(
            "/api/oauth/device_authorization",
            post(device_authorization),
        )
        .route("/api/oauth/token", post(device_token))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
    }
    let store = Store::new(&path, format!("http://{address}")).unwrap();
    let authorization = store.begin_device_login().await.unwrap();
    assert_eq!(authorization.user_code, "ABCD-EFGH");
    assert_eq!(
        authorization.verification_uri_complete,
        "https://example.test/activate"
    );
    store.finish_device_login(&authorization).await.unwrap();

    let saved: StoredAuth = serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
    assert_eq!(saved.access, "device-access");
    assert_eq!(saved.refresh, "device-refresh");
    assert_eq!(*state.polls.lock().await, 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).await.unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(dir.path()).await.unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

async fn refresh(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Json<Value> {
    state.forms.lock().await.push(form);
    state.headers.lock().await.push(headers);
    Json(json!({
        "access_token": "fresh-access",
        "refresh_token": "rotated-refresh",
        "expires_in": 900
    }))
}

async fn device_authorization() -> Json<Value> {
    Json(json!({
        "user_code": "ABCD-EFGH",
        "device_code": "device-code",
        "verification_uri": "https://example.test",
        "verification_uri_complete": "https://example.test/activate",
        "expires_in": 30,
        "interval": 0
    }))
}

async fn device_token(
    State(state): State<OAuthState>,
    Form(_form): Form<HashMap<String, String>>,
) -> (axum::http::StatusCode, Json<Value>) {
    let mut polls = state.polls.lock().await;
    *polls += 1;
    if *polls == 1 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "authorization_pending"})),
        );
    }
    (
        axum::http::StatusCode::OK,
        Json(json!({
            "access_token": "device-access",
            "refresh_token": "device-refresh",
            "expires_in": 900,
            "scope": "openid"
        })),
    )
}

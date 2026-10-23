use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lrtodo_core::{Change, Replica, Task};
use tower::ServiceExt;

use super::*;

async fn app(token: Option<&str>) -> (tempfile::TempDir, Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        data_dir: dir.path().to_path_buf(),
        state_dir: None,
        bind: default_bind(),
        token: token.map(str::to_string),
        ha: None,
        trust_ingress: true,
    };
    let app = App::new(&cfg).await.unwrap();
    app.store
        .create_list("compras", Some("Compras"), Layout::File)
        .unwrap();
    (dir, app)
}

async fn call(app: &Arc<App>, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = router(app.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    (
        status,
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

async fn sync(app: &Arc<App>, r: &mut Replica, now: u64) {
    let body = serde_json::to_vec(&r.sync_request()).unwrap();
    let req = Request::post("/api/sync")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let (status, body) = call(app, req).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    r.apply_response(serde_json::from_slice(&body).unwrap(), now);
}

#[tokio::test]
async fn sync_roundtrip_over_http() {
    let (dir, app) = app(None).await;
    let mut phone = Replica::new("phone", 1);
    let mut laptop = Replica::new("laptop", 2);
    sync(&app, &mut phone, 1).await;
    sync(&app, &mut laptop, 1).await;
    assert_eq!(phone.lists()[0].info.meta.name, "Compras");

    let now = now_ms();
    let leche = phone.add("compras", Task::new("", "leche"), now).unwrap();
    phone.add("compras", Task::new("", "pan"), now + 1).unwrap();
    sync(&app, &mut phone, now + 2).await;
    assert_eq!(phone.pending(), 0);

    // Se escribió el YAML.
    let yaml = std::fs::read_to_string(dir.path().join("compras.yaml")).unwrap();
    assert!(
        yaml.contains("title: leche") && yaml.contains("title: pan"),
        "{yaml}"
    );

    sync(&app, &mut laptop, now + 3).await;
    laptop
        .set("compras", &leche, Change::Done(true), now + 4)
        .unwrap();
    sync(&app, &mut laptop, now + 5).await;
    sync(&app, &mut phone, now + 6).await;
    let view = phone.view("compras").unwrap();
    assert!(view.iter().any(|t| t.task.id == leche && t.task.done));

    // Edición a mano del YAML: llega a los clientes.
    let yaml = std::fs::read_to_string(dir.path().join("compras.yaml")).unwrap();
    std::fs::write(
        dir.path().join("compras.yaml"),
        yaml.replace("title: pan", "title: pan integral"),
    )
    .unwrap();
    sync(&app, &mut phone, now + 7).await;
    assert!(
        phone
            .view("compras")
            .unwrap()
            .iter()
            .any(|t| t.task.title == "pan integral")
    );
}

#[tokio::test]
async fn broken_file_keeps_client_cache_and_ops() {
    let (dir, app) = app(None).await;
    let mut phone = Replica::new("phone", 1);
    sync(&app, &mut phone, 1).await;
    let now = now_ms();
    phone.add("compras", Task::new("", "leche"), now).unwrap();
    sync(&app, &mut phone, now + 1).await;

    std::fs::write(dir.path().join("compras.yaml"), "items: [\n").unwrap();
    phone.add("compras", Task::new("", "pan"), now + 2).unwrap();
    sync(&app, &mut phone, now + 3).await;
    // La op queda pendiente, el cliente sigue viendo todo, y se informa el error.
    assert_eq!(phone.pending(), 1);
    assert_eq!(phone.view("compras").unwrap().len(), 2);
    assert!(phone.lists()[0].info.error.is_some());
}

#[tokio::test]
async fn unknown_list_ops_are_rejected() {
    let (_dir, app) = app(None).await;
    let mut phone = Replica::new("phone", 1);
    sync(&app, &mut phone, 1).await;
    phone
        .add("compras", Task::new("", "leche"), now_ms())
        .unwrap();
    std::fs::remove_file(app.store.root().join("compras.yaml")).unwrap();
    sync(&app, &mut phone, now_ms()).await;
    assert_eq!(phone.pending(), 0);
    assert!(phone.lists().is_empty());
    assert!(!phone.notices.is_empty());
}

#[tokio::test]
async fn token_required() {
    let (_dir, app) = app(Some("secreto")).await;
    let get = |uri: &str, auth: Option<&str>| {
        let mut b = Request::get(uri);
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        b.body(Body::empty()).unwrap()
    };
    assert_eq!(
        call(&app, get("/api/lists", None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, get("/api/lists", Some("Bearer nop"))).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, get("/api/lists", Some("Bearer secreto")))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, get("/api/lists?token=secreto", None)).await.0,
        StatusCode::OK
    );
    assert_eq!(call(&app, get("/api/health", None)).await.0, StatusCode::OK);

    // Ingress de HA: sólo si viene del proxy del Supervisor.
    let ingress = |ip: [u8; 4]| {
        let mut req = Request::get("/api/lists")
            .header("x-ingress-path", "/api/hassio_ingress/abc")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                ip, 1234,
            ))));
        req
    };
    assert_eq!(
        call(&app, ingress([172, 30, 32, 2])).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, ingress([192, 168, 1, 50])).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn create_list_endpoint() {
    let (dir, app) = app(None).await;
    let req = |body: &str| {
        Request::post("/api/lists")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (status, _) = call(
        &app,
        req(r#"{"id": "casa", "name": "Casa", "layout": "dir"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(dir.path().join("casa/_list.yaml").is_file());
    assert_eq!(
        call(&app, req(r#"{"id": "casa"}"#)).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, req(r#"{"id": "../x"}"#)).await.0,
        StatusCode::BAD_REQUEST
    );
}

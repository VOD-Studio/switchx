use super::*;
use switchx::request_overrides::RequestOverrides;

#[tokio::test]
async fn custom_requests_preserve_routing_auth_and_streaming_and_isolate_models() {
    let (tx, mut seen) = mpsc::channel::<(HeaderMap, Value)>(8);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/responses", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                tx.send((headers, body.clone())).await.unwrap();
                if body["stream"] == true {
                    ([(header::CONTENT_TYPE, "text/event-stream")], "event: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n").into_response()
                } else { Json(json!({"status":"completed","output":[]})).into_response() }
            }
        }))).await.unwrap();
    });
    let templates =
        serde_json::from_str(include_str!("../fixtures/synthetic-models.json")).unwrap();
    let mut publication = publish(
        &templates,
        &[
            Selection {
                public_id: "custom",
                display_name: "Custom",
                provider_id: "one",
                upstream_model: "deepseek-flash",
            },
            Selection {
                public_id: "plain",
                display_name: "Plain",
                provider_id: "one",
                upstream_model: "deepseek-flash",
            },
        ],
    )
    .unwrap();
    publication
        .routes
        .get_mut("custom")
        .unwrap()
        .request_overrides = RequestOverrides::parse(
        "synthetic-client/1",
        r#"{"User-Agent":"override-client/2","X-Tenant":"private-tenant"}"#,
        r#"{"temperature":0.2,"reasoning":{"effort":"high"},"metadata":{"test":"private-body"}}"#,
    )
    .unwrap();
    publication.add_alias("legacy", "custom").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let records = RequestDatabase::new();
    let state = RouterState::new(
        address,
        RECORD_TOKEN.into(),
        publication,
        HashMap::from([(
            "one".into(),
            Upstream::new(
                &format!("http://{upstream_address}"),
                "synthetic-provider-key".into(),
            )
            .unwrap(),
        )]),
    )
    .unwrap()
    .with_request_log(records.store(), "customization".into());
    let router = RunningRouter::start(listener, state).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for (model, streaming) in [("custom", false), ("legacy", true), ("plain", false)] {
        let response = request(&client, address, json!({"model":model,"stream":streaming,"input":"hello","temperature":0.8,"reasoning":{"effort":"low","summary":"auto"}}))
            .header("User-Agent", "incoming-client/0").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let content = response.text().await.unwrap();
        if streaming {
            assert!(content.starts_with("event: response.completed\n"));
        }
        let (headers, body) = seen.recv().await.unwrap();
        assert_eq!(
            headers[header::AUTHORIZATION],
            "Bearer synthetic-provider-key"
        );
        assert!(!headers.contains_key(LOCAL_TOKEN_HEADER));
        assert_eq!(body["model"], "deepseek-flash");
        assert_eq!(body["input"], "hello");
        assert_eq!(body["stream"], streaming);
        assert_eq!(body["reasoning"]["summary"], "auto");
        if model == "plain" {
            assert!(!headers.contains_key("x-tenant"));
            assert_ne!(
                headers.get("user-agent").and_then(|v| v.to_str().ok()),
                Some("override-client/2")
            );
            assert_eq!(body["temperature"], 0.8);
            assert!(body.get("metadata").is_none());
        } else {
            assert_eq!(headers["user-agent"], "override-client/2");
            assert_eq!(headers["x-tenant"], "private-tenant");
            assert_eq!(body["temperature"], 0.2);
            assert_eq!(body["reasoning"]["effort"], "high");
        }
        let record = records.wait_for(&id).await;
        assert_eq!(record.status, RequestStatus::Completed);
        let log = format!("{record:?}");
        assert!(!log.contains("private-tenant") && !log.contains("private-body"));
    }
    router.stop().await;
    upstream.abort();
}

#[tokio::test]
async fn fallback_uses_only_its_own_request_customization() {
    let (tx, mut seen) = mpsc::channel(1);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup_address = listener.local_addr().unwrap();
    let backup = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/responses",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let tx = tx.clone();
                    async move {
                        tx.send((headers, body)).await.unwrap();
                        Json(json!({"status":"completed","output":[]}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_address = unused.local_addr().unwrap();
    drop(unused);
    let templates =
        serde_json::from_str(include_str!("../fixtures/synthetic-models.json")).unwrap();
    let mut publication = publish(
        &templates,
        &[Selection {
            public_id: "custom",
            display_name: "Custom",
            provider_id: "one",
            upstream_model: "deepseek-flash",
        }],
    )
    .unwrap();
    let binding = publication.routes.get_mut("custom").unwrap();
    binding.fallback_provider_id = Some("backup".into());
    binding.request_overrides = RequestOverrides::parse(
        "primary/1",
        r#"{"x-primary":"private"}"#,
        r#"{"temperature":0.1,"metadata":{"primary":true}}"#,
    )
    .unwrap();
    binding.fallback_request_overrides = RequestOverrides::parse(
        "backup/2",
        r#"{"x-backup":"backup"}"#,
        r#"{"temperature":0.4}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = RouterState::new(
        address,
        RECORD_TOKEN.into(),
        publication,
        HashMap::from([
            (
                "one".into(),
                Upstream::new(&format!("http://{primary_address}"), "primary-key".into()).unwrap(),
            ),
            (
                "backup".into(),
                Upstream::new(&format!("http://{backup_address}"), "backup-key".into()).unwrap(),
            ),
        ]),
    )
    .unwrap();
    let router = RunningRouter::start(listener, state).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        request(&client, address, json!({"model":"custom","input":"hello"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let (headers, body) = seen.recv().await.unwrap();
    assert_eq!(headers["authorization"], "Bearer backup-key");
    assert_eq!(headers["user-agent"], "backup/2");
    assert!(!headers.contains_key("x-primary"));
    assert_eq!(body["temperature"], 0.4);
    assert!(body.get("metadata").is_none());
    router.stop().await;
    backup.abort();
}

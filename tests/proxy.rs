use std::{collections::HashMap, time::Duration};

use serde_json::json;
use switchx::{
    catalog::{Selection, publish},
    direct,
    routing::{RouterState, RunningRouter, Upstream},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    sync::mpsc,
    time::timeout,
};

const CHILD_ENV: &str = "SWITCHX_PROXY_TEST_CHILD";
const LOCAL_TOKEN: &str = "synthetic-local-router-token-at-least-32-bytes";
const API_KEY: &str = "synthetic-upstream-api-key";

async fn probe_checks_and_routes() {
    // This reserved domain must only reach the local proxy, never a real upstream.
    let base_url = "https://switchx-proxy.test/v1";
    assert!(direct::fetch_models(base_url, API_KEY).await.is_err());

    let templates = serde_json::from_str(include_str!("fixtures/synthetic-models.json")).unwrap();
    let publication = publish(
        &templates,
        &[Selection {
            public_id: "sx-proxy",
            display_name: "Proxy fixture",
            provider_id: "proxy",
            upstream_model: "gpt-5.5",
        }],
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = RouterState::new(
        address,
        LOCAL_TOKEN.into(),
        publication,
        HashMap::from([(
            "proxy".into(),
            Upstream::new(base_url, API_KEY.into()).unwrap(),
        )]),
    )
    .unwrap();
    let router = RunningRouter::start(listener, state).unwrap();
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{address}/v1/responses"))
        .bearer_auth(LOCAL_TOKEN)
        .header("content-type", "application/json")
        .body(json!({"model":"sx-proxy", "input":[]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    router.stop().await;
}

#[tokio::test]
async fn model_checks_and_routed_requests_honor_proxy_and_no_proxy() {
    if std::env::var_os(CHILD_ENV).is_some() {
        probe_checks_and_routes().await;
        return;
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let (seen, mut requests) = mpsc::channel(8);
    let proxy = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0 && request.len() + count <= 8192);
                request.extend_from_slice(&buffer[..count]);
            }
            seen.send(String::from_utf8(request).unwrap())
                .await
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        }
    });

    for bypass in [false, true] {
        // Child processes isolate proxy variables from Rust's parallel test threads.
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "model_checks_and_routed_requests_honor_proxy_and_no_proxy",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env_remove("REQUEST_METHOD")
            .kill_on_drop(true);
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            child.env(name, &proxy_url);
        }
        for name in ["NO_PROXY", "no_proxy"] {
            child.env(
                name,
                if bypass {
                    "127.0.0.1,localhost,switchx-proxy.test"
                } else {
                    "127.0.0.1,localhost"
                },
            );
        }
        let output = timeout(Duration::from_secs(20), child.output())
            .await
            .expect("proxy probe timed out")
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for _ in 0..if bypass { 0 } else { 2 } {
            let request = requests
                .try_recv()
                .expect("model checks and routed requests must both use the proxy");
            assert!(request.starts_with("CONNECT switchx-proxy.test:443 HTTP/1.1\r\n"));
            assert!(!request.contains(LOCAL_TOKEN));
            assert!(!request.contains(API_KEY));
        }
        assert!(requests.try_recv().is_err());
    }
    proxy.abort();
}

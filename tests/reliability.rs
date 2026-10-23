use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rchain_observer::{
    config::{parse_node, validate_targets},
    http::{get_json, SNAPSHOT_BODY_LIMIT},
    model::{NodeTarget, Observation, VerdictClass},
    observer::Observer,
    service::{self, ApiState, CachedReport},
    verdict::classify,
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::RwLock};

fn row(name: &str, height: u64, hash: char) -> Observation {
    Observation {
        node: name.into(),
        base_url: format!("http://{name}.example"),
        reachable: true,
        network_id: Some("testnet".into()),
        shard_id: Some("/root".into()),
        latest_block: Some(height + 2),
        finalized_block: Some(height),
        finalized_hash: Some(hash.to_string().repeat(64)),
        error: None,
    }
}

#[test]
fn malformed_or_failed_rows_never_vote() {
    let good = row("a", 10, 'a');
    for field in 0..6 {
        let mut bad = row("b", 10, 'b');
        match field {
            0 => bad.reachable = false,
            1 => bad.error = Some("bad sample".into()),
            2 => bad.network_id = Some(" ".into()),
            3 => bad.shard_id = Some("".into()),
            4 => bad.finalized_hash = Some("abc".into()),
            _ => bad.finalized_hash = Some("g".repeat(64)),
        }
        let verdict = classify(&[good.clone(), bad]);
        assert_eq!(verdict.comparable_nodes, 1);
        assert_eq!(verdict.observed_nodes, 2);
    }
    assert_eq!(classify(&[]).class, VerdictClass::Unavailable);
}

#[test]
fn repeated_labels_are_excluded_and_hex_case_is_canonical() {
    let a = row("a", 10, 'a');
    let mut b = row("b", 10, 'a');
    b.finalized_hash = Some("A".repeat(64));
    assert_eq!(
        classify(&[a.clone(), b.clone()]).class,
        VerdictClass::Agreement
    );
    let verdict = classify(&[a.clone(), a, b]);
    assert_eq!(verdict.comparable_nodes, 1);
}

#[test]
fn highest_height_cannot_hide_a_lower_strict_majority() {
    let rows = vec![
        row("a", 100, 'a'),
        row("b", 100, 'a'),
        row("c", 100, 'a'),
        row("d", 100, 'b'),
        row("e", 101, 'c'),
    ];
    let expected = classify(&rows);
    assert_eq!(expected.class, VerdictClass::Majority);
    assert_eq!(expected.agreeing_nodes, 3);
    assert_eq!(expected.reference.as_ref().unwrap().block_number, 100);
    for shift in 0..rows.len() {
        let mut permutation = rows.clone();
        permutation.rotate_left(shift);
        assert_eq!(classify(&permutation), expected);
        permutation.reverse();
        assert_eq!(classify(&permutation), expected);
    }
}

#[test]
fn reference_ties_have_a_total_deterministic_order() {
    let a = row("a", 10, 'a');
    let mut b = row("b", 10, 'a');
    b.network_id = Some("other".into());
    assert_eq!(classify(&[a.clone(), b.clone()]), classify(&[b, a]));
}

#[test]
fn configuration_rejects_ambiguous_targets() {
    for input in [
        "a=ftp://host",
        "=http://host",
        "a=http://u:password@host",
        "a=http://host/?q=1",
        "a=http://host/#part",
        "a=http://",
        "two words=http://host",
    ] {
        assert!(parse_node(input).is_err(), "{input}");
    }
    let a = parse_node("a=http://EXAMPLE.com:80/").unwrap();
    assert_eq!(a.base_url, "http://example.com");
    let b = parse_node("b=http://example.com").unwrap();
    assert!(validate_targets(vec![a.clone(), b]).is_err());
    assert!(validate_targets(vec![]).is_err());
    assert!(Observer::new(vec![a], Duration::ZERO).is_err());
}

async fn spawn(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}

async fn sample(status: Value, finalized: Value) -> rchain_observer::model::ObservationReport {
    let (url, task) = spawn(
        Router::new()
            .route(
                "/api/status",
                get(move || {
                    let value = status.clone();
                    async move { Json(value) }
                }),
            )
            .route(
                "/api/last-finalized-block",
                get(move || {
                    let value = finalized.clone();
                    async move { Json(value) }
                }),
            ),
    )
    .await;
    let report = Observer::new(
        vec![NodeTarget {
            name: "a".into(),
            base_url: url,
        }],
        Duration::from_secs(1),
    )
    .unwrap()
    .observe()
    .await;
    task.abort();
    report
}

#[tokio::test]
async fn snapshot_identity_validation_and_topology_redaction() {
    let status = json!({"networkId":"testnet","shardId":"/root","latestBlockNumber":10});
    let finalized =
        json!({"blockInfo":{"blockNumber":8,"blockHash":"A".repeat(64),"shardId":"/root"}});
    let report = sample(status.clone(), finalized.clone()).await;
    assert_eq!(report.verdict.class, VerdictClass::Agreement);
    assert_eq!(report.observations[0].finalized_hash, Some("a".repeat(64)));
    assert!(report.completed_at_ms >= report.observed_at_ms);
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("127.0.0.1"));
    let mut wrong = finalized;
    wrong["blockInfo"]["shardId"] = json!("/wrong");
    let report = sample(status.clone(), wrong).await;
    assert_eq!(report.verdict.class, VerdictClass::Unavailable);
    assert!(report.observations[0]
        .error
        .as_ref()
        .unwrap()
        .contains("different shards"));
    let report = sample(
        status,
        json!({"blockInfo":{"blockNumber":8,"blockHash":"invalid"}}),
    )
    .await;
    assert_eq!(report.verdict.comparable_nodes, 0);
    assert!(report.observations[0].error.is_some());
}

#[tokio::test]
async fn bodies_are_bounded_even_without_content_length() {
    let (url, task) = spawn(Router::new().route(
        "/",
        get(|| async {
            let chunks = futures::stream::iter(
                (0..3).map(|_| Ok::<_, std::io::Error>(vec![b' '; SNAPSHOT_BODY_LIMIT / 2])),
            );
            Response::new(Body::from_stream(chunks))
        }),
    ))
    .await;
    let error = get_json::<Value>(&reqwest::Client::new(), &url, SNAPSHOT_BODY_LIMIT)
        .await
        .unwrap_err();
    assert!(error.contains("body limit"));
    assert!(!error.contains(&url));
    task.abort();
}

#[tokio::test]
async fn redirect_and_private_error_bodies_are_not_followed_or_published() {
    let (url, task) = spawn(
        Router::new()
            .route(
                "/api/status",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "http://private.example/internal")],
                        "private upstream detail",
                    )
                        .into_response()
                }),
            )
            .route(
                "/api/last-finalized-block",
                get(|| async { Json(json!({})) }),
            ),
    )
    .await;
    let report = Observer::new(
        vec![NodeTarget {
            name: "a".into(),
            base_url: url.clone(),
        }],
        Duration::from_secs(1),
    )
    .unwrap()
    .observe()
    .await;
    assert_eq!(report.verdict.class, VerdictClass::Unavailable);
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(encoded.contains("HTTP 302"));
    assert!(!encoded.contains("private.example"));
    assert!(!encoded.contains(&url));
    task.abort();
}

#[tokio::test]
async fn body_timeout_covers_the_entire_read() {
    let (url, task) = spawn(Router::new().route(
        "/",
        get(|| async {
            let stream = futures::stream::once(async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok::<_, std::io::Error>("{}")
            });
            Response::new(Body::from_stream(stream))
        }),
    ))
    .await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(40))
        .build()
        .unwrap();
    let error = get_json::<Value>(&client, &url, 100).await.unwrap_err();
    assert!(error.contains("timed out"));
    task.abort();
}

#[tokio::test]
async fn health_readiness_freshness_and_http_boundary_are_distinct() {
    let mut report = sample(
        json!({"networkId":"testnet","shardId":"/root"}),
        json!({"blockInfo":{"blockNumber":8,"blockHash":"a".repeat(64)}}),
    )
    .await;
    // A singleton never satisfies multi-node readiness.
    let state = ApiState {
        snapshot: Arc::new(RwLock::new(CachedReport {
            report: report.clone(),
            sampled: Instant::now(),
        })),
        stale_after: Duration::from_secs(30),
    };
    let (url, task) = spawn(service::router(state.clone())).await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{url}/ready"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    report.observations.push(row("b", 8, 'a'));
    report.verdict = classify(&report.observations);
    report.heights.reachable_nodes = 2;
    state.snapshot.write().await.report = report;
    let ready = client.get(format!("{url}/ready")).send().await.unwrap();
    assert_eq!(ready.status(), 200);
    assert_eq!(ready.headers()["cache-control"], "no-store");
    assert_eq!(ready.headers()["x-content-type-options"], "nosniff");
    state.snapshot.write().await.sampled = Instant::now() - Duration::from_secs(31);
    let health = client.get(format!("{url}/health")).send().await.unwrap();
    assert_eq!(health.status(), 200);
    let health: Value = health.json().await.unwrap();
    assert_eq!(health["status"], "stale");
    assert_eq!(health["ready"], false);
    assert_eq!(
        client
            .get(format!("{url}/ready"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        client
            .post(format!("{url}/v1/observation"))
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert_eq!(
        client
            .get(format!("{url}/api/deploy"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    task.abort();
}

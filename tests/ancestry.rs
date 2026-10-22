use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use k256::SecretKey;
use prost::Message;
use rchain_observer::{
    model::{AncestryClass, NodeTarget, VerdictClass},
    observer::Observer,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;

#[derive(Clone)]
struct MockNode {
    finalized_height: u64,
    finalized_hash: String,
    raw_blocks: Arc<HashMap<String, Value>>,
    fail_hash: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct TestBlockProto {
    #[prost(int32, tag = "1")]
    version: i32,
    #[prost(string, tag = "2")]
    shard_id: String,
    #[prost(bytes = "vec", tag = "3")]
    block_hash: Vec<u8>,
    #[prost(int64, tag = "4")]
    block_number: i64,
    #[prost(bytes = "vec", tag = "5")]
    sender: Vec<u8>,
    #[prost(int64, tag = "6")]
    seq_num: i64,
    #[prost(bytes = "vec", tag = "7")]
    pre_state_hash: Vec<u8>,
    #[prost(bytes = "vec", tag = "8")]
    post_state_hash: Vec<u8>,
    #[prost(bytes = "vec", repeated, tag = "9")]
    justifications: Vec<Vec<u8>>,
    #[prost(string, tag = "15")]
    sig_algorithm: String,
    #[prost(bytes = "vec", tag = "16")]
    sig: Vec<u8>,
    #[prost(int64, tag = "17")]
    timestamp: i64,
}

#[derive(Clone)]
struct RawFixture {
    hash: String,
    response: Value,
}

async fn status(State(state): State<MockNode>) -> Json<Value> {
    Json(json!({
        "networkId": "testnet",
        "shardId": "/root",
        "latestBlockNumber": state.finalized_height + 2
    }))
}

async fn finalized(State(state): State<MockNode>) -> Json<Value> {
    Json(json!({
        "blockInfo": {
            "blockNumber": state.finalized_height,
            "blockHash": state.finalized_hash,
            "shardId": "/root"
        }
    }))
}

async fn raw_block(Path(hash): Path<String>, State(state): State<MockNode>) -> Response {
    if state.fail_hash.as_deref() == Some(hash.as_str()) {
        return (StatusCode::SERVICE_UNAVAILABLE, "evidence unavailable").into_response();
    }
    match state.raw_blocks.get(&hash) {
        Some(value) => Json(value.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "unknown block").into_response(),
    }
}

async fn spawn_node(
    finalized_height: u64,
    finalized_hash: &str,
    raw_blocks: HashMap<String, Value>,
    fail_hash: Option<&str>,
) -> NodeTarget {
    let state = MockNode {
        finalized_height,
        finalized_hash: finalized_hash.to_string(),
        raw_blocks: Arc::new(raw_blocks),
        fail_hash: fail_hash.map(str::to_string),
    };
    let app = Router::new()
        .route("/api/status", get(status))
        .route("/api/last-finalized-block", get(finalized))
        .route("/api/v1/block/{hash}/raw", get(raw_block))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    NodeTarget {
        name: format!("node-{finalized_height}-{}", &finalized_hash[..8]),
        base_url: format!("http://{address}"),
    }
}

fn raw_fixture(height: u64, justifications: &[String]) -> RawFixture {
    raw_fixture_with_timestamp(height, justifications, 1_790_000_000_000)
}

fn raw_fixture_with_timestamp(
    height: u64,
    justifications: &[String],
    timestamp: i64,
) -> RawFixture {
    raw_fixture_with_algorithm(height, justifications, timestamp, "secp256k1")
}

fn raw_fixture_with_algorithm(
    height: u64,
    justifications: &[String],
    timestamp: i64,
    algorithm: &str,
) -> RawFixture {
    let secret = SecretKey::from_slice(&[3u8; 32]).unwrap();
    let signing = SigningKey::from(secret);
    let sender = signing
        .verifying_key()
        .to_sec1_point(false)
        .as_bytes()
        .to_vec();
    let proto = TestBlockProto {
        version: 1,
        shard_id: "/root".to_string(),
        block_hash: vec![0u8; 32],
        block_number: height as i64,
        sender: sender.clone(),
        seq_num: height as i64,
        pre_state_hash: vec![1u8; 32],
        post_state_hash: vec![2u8; 32],
        justifications: justifications
            .iter()
            .map(|value| hex::decode(value).unwrap())
            .collect(),
        sig_algorithm: algorithm.to_string(),
        sig: Vec::new(),
        timestamp,
    };
    let encoded = proto.encode_to_vec();
    let hash = blake2b256_hex(&encoded);
    let hash_bytes = hex::decode(&hash).unwrap();
    let signature: Signature = signing.sign_prehash(&hash_bytes).unwrap();
    let signature_bytes = match algorithm {
        "secp256k1" => signature.to_der().as_bytes().to_vec(),
        "secp256k1:eth" => signature.to_bytes().to_vec(),
        other => panic!("unsupported test algorithm: {other}"),
    };
    RawFixture {
        hash: hash.clone(),
        response: json!({
            "blockHash": hash,
            "encoded": hex::encode(encoded),
            "sender": hex::encode(sender),
            "sigAlgorithm": algorithm,
            "sig": hex::encode(signature_bytes)
        }),
    }
}

fn blake2b256_hex(input: &[u8]) -> String {
    let mut hasher = Blake2bVar::new(32).unwrap();
    hasher.update(input);
    let mut output = [0u8; 32];
    hasher.finalize_variable(&mut output).unwrap();
    hex::encode(output)
}

async fn observe_pair(
    lower: &RawFixture,
    higher: &RawFixture,
    higher_raw_blocks: HashMap<String, Value>,
    fail_hash: Option<&str>,
) -> rchain_observer::model::ObservationReport {
    let lower_node = spawn_node(99, &lower.hash, HashMap::new(), None).await;
    let higher_node = spawn_node(100, &higher.hash, higher_raw_blocks, fail_hash).await;
    Observer::new(vec![lower_node, higher_node], Duration::from_secs(1))
        .unwrap()
        .with_ancestry_max_blocks(32)
        .observe()
        .await
}

#[tokio::test]
async fn raw_preimages_prove_lag_and_supply_all_ancestry_fields() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, std::slice::from_ref(&lower.hash));
    let blocks = HashMap::from([
        (higher.hash.clone(), higher.response.clone()),
        (lower.hash.clone(), lower.response.clone()),
    ]);
    let report = observe_pair(&lower, &higher, blocks, None).await;

    assert_eq!(report.verdict.class, VerdictClass::ProvenLag);
    assert_eq!(report.ancestry_checks.len(), 1);
    let check = &report.ancestry_checks[0];
    assert_eq!(check.class, AncestryClass::ProvenLag);
    assert_eq!(check.path, vec![higher.hash, lower.hash]);
    assert_eq!(check.examined_blocks, 2);
    assert_eq!(check.cryptographically_verified_blocks, 2);
    assert!(check.reason.contains("BLAKE2b-256-verified"));
}

#[tokio::test]
async fn complete_verified_dag_without_lower_hash_is_conflicting_history() {
    let lower = raw_fixture(99, &[]);
    // Make the second height-99 block distinct while preserving a valid raw preimage.
    let other = raw_fixture_with_timestamp(99, &[], 1_790_000_000_001);
    let higher = raw_fixture(100, std::slice::from_ref(&other.hash));
    let blocks = HashMap::from([
        (higher.hash.clone(), higher.response.clone()),
        (other.hash.clone(), other.response.clone()),
    ]);
    let report = observe_pair(&lower, &higher, blocks, None).await;

    assert_eq!(
        report.verdict.class,
        VerdictClass::ConflictingFinalizedHistory
    );
    let check = &report.ancestry_checks[0];
    assert_eq!(check.class, AncestryClass::ConflictingFinalizedHistory);
    assert_eq!(check.cryptographically_verified_blocks, 2);
}

#[tokio::test]
async fn missing_raw_block_evidence_keeps_height_skew_unresolved() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, std::slice::from_ref(&lower.hash));
    let blocks = HashMap::from([(higher.hash.clone(), higher.response.clone())]);
    let report = observe_pair(&lower, &higher, blocks, Some(&lower.hash)).await;

    assert_eq!(report.verdict.class, VerdictClass::HeightSkew);
    let check = &report.ancestry_checks[0];
    assert_eq!(check.class, AncestryClass::UnresolvedHeightSkew);
    assert_eq!(check.cryptographically_verified_blocks, 1);
    assert!(check.reason.contains("HTTP 503"));
}

#[tokio::test]
async fn tampered_raw_preimage_is_never_accepted_as_ancestry() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, std::slice::from_ref(&lower.hash));
    let mut tampered = higher.response.clone();
    let encoded = tampered["encoded"].as_str().unwrap();
    let mut bytes = hex::decode(encoded).unwrap();
    *bytes.last_mut().unwrap() ^= 0x01;
    tampered["encoded"] = Value::String(hex::encode(bytes));
    let blocks = HashMap::from([(higher.hash.clone(), tampered)]);
    let report = observe_pair(&lower, &higher, blocks, None).await;

    assert_eq!(report.verdict.class, VerdictClass::HeightSkew);
    let check = &report.ancestry_checks[0];
    assert_eq!(check.cryptographically_verified_blocks, 0);
    assert!(check.reason.contains("BLAKE2b-256"));
}

#[tokio::test]
async fn secp256k1_eth_raw_signatures_are_verified() {
    let lower = raw_fixture_with_algorithm(99, &[], 1_790_000_000_010, "secp256k1:eth");
    let higher = raw_fixture_with_algorithm(
        100,
        std::slice::from_ref(&lower.hash),
        1_790_000_000_011,
        "secp256k1:eth",
    );
    let blocks = HashMap::from([
        (higher.hash.clone(), higher.response.clone()),
        (lower.hash.clone(), lower.response.clone()),
    ]);
    let report = observe_pair(&lower, &higher, blocks, None).await;

    assert_eq!(report.verdict.class, VerdictClass::ProvenLag);
    assert_eq!(
        report.ancestry_checks[0].cryptographically_verified_blocks,
        2
    );
}

#[tokio::test]
async fn invalid_detached_signature_is_never_accepted_as_ancestry() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, std::slice::from_ref(&lower.hash));
    let mut bad_sig = higher.response.clone();
    let sig_hex = bad_sig["sig"].as_str().unwrap();
    let mut sig = hex::decode(sig_hex).unwrap();
    let last = sig.len() - 1;
    sig[last] ^= 0x01;
    bad_sig["sig"] = Value::String(hex::encode(sig));
    let blocks = HashMap::from([(higher.hash.clone(), bad_sig)]);
    let report = observe_pair(&lower, &higher, blocks, None).await;

    assert_eq!(report.verdict.class, VerdictClass::HeightSkew);
    let check = &report.ancestry_checks[0];
    assert_eq!(check.cryptographically_verified_blocks, 0);
    assert!(check.reason.contains("signature"));
}

#[tokio::test]
async fn outer_sender_must_match_sender_committed_by_verified_preimage() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, std::slice::from_ref(&lower.hash));
    let mut mismatched = higher.response.clone();
    mismatched["sender"] = Value::String(hex::encode(vec![7u8; 65]));
    let blocks = HashMap::from([(higher.hash.clone(), mismatched)]);
    let report = observe_pair(&lower, &higher, blocks, None).await;

    assert_eq!(report.verdict.class, VerdictClass::HeightSkew);
    assert!(report.ancestry_checks[0].reason.contains("sender differs"));
}

#[tokio::test]
async fn a_wide_frontier_cannot_allocate_beyond_the_unique_block_budget() {
    let lower = raw_fixture(99, &[]);
    let parents = (0..40u8)
        .map(|byte| hex::encode([byte; 32]))
        .collect::<Vec<_>>();
    let higher = raw_fixture(100, &parents);
    let report = observe_pair(
        &lower,
        &higher,
        HashMap::from([(higher.hash.clone(), higher.response.clone())]),
        None,
    )
    .await;
    assert_eq!(report.verdict.class, VerdictClass::HeightSkew);
    assert_eq!(report.ancestry_checks[0].examined_blocks, 1);
    assert!(report.ancestry_checks[0].reason.contains("frontier"));
}

#[tokio::test]
async fn duplicate_justifications_are_fetched_once_and_the_path_is_preserved() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, &[lower.hash.clone(), lower.hash.clone()]);
    let report = observe_pair(
        &lower,
        &higher,
        HashMap::from([
            (higher.hash.clone(), higher.response.clone()),
            (lower.hash.clone(), lower.response.clone()),
        ]),
        None,
    )
    .await;
    assert_eq!(report.verdict.class, VerdictClass::ProvenLag);
    assert_eq!(report.ancestry_checks[0].examined_blocks, 2);
    assert_eq!(
        report.ancestry_checks[0].path,
        vec![higher.hash, lower.hash]
    );
}

#[tokio::test]
async fn an_excessive_justification_fanout_is_unresolved() {
    let lower = raw_fixture(99, &[]);
    let higher = raw_fixture(100, &vec![lower.hash.clone(); 1025]);
    let report = observe_pair(
        &lower,
        &higher,
        HashMap::from([(higher.hash.clone(), higher.response.clone())]),
        None,
    )
    .await;
    assert_eq!(report.verdict.class, VerdictClass::HeightSkew);
    assert!(report.ancestry_checks[0]
        .reason
        .contains("1024-justification"));
}

use rchain_observer::{
    model::{Observation, VerdictClass},
    verdict::classify,
};

fn observed(node: &str, block: u64, hash: &str) -> Observation {
    Observation {
        node: node.to_string(),
        base_url: format!("https://{node}.example"),
        reachable: true,
        network_id: Some("testnet".to_string()),
        shard_id: Some("/root".to_string()),
        latest_block: Some(block + 2),
        finalized_block: Some(block),
        finalized_hash: Some(format!("{hash:0<64}")),
        error: None,
    }
}

#[test]
fn unanimous_snapshot_is_agreement() {
    let observations = vec![observed("a", 100, "abc"), observed("b", 100, "abc")];
    let verdict = classify(&observations);
    assert_eq!(verdict.class, VerdictClass::Agreement);
    assert_eq!(verdict.agreeing_nodes, 2);
}

#[test]
fn different_finalized_heights_are_height_skew_not_hash_divergence() {
    let observations = vec![
        observed("a", 100, "abc"),
        observed("b", 100, "abc"),
        observed("c", 99, "def"),
    ];
    let verdict = classify(&observations);
    assert_eq!(verdict.class, VerdictClass::HeightSkew);
    assert_eq!(verdict.agreeing_nodes, 2);
}

#[test]
fn strict_majority_at_same_height_with_conflicting_hash_is_reported() {
    let observations = vec![
        observed("a", 100, "abc"),
        observed("b", 100, "abc"),
        observed("c", 100, "def"),
    ];
    let verdict = classify(&observations);
    assert_eq!(verdict.class, VerdictClass::Majority);
    assert_eq!(verdict.agreeing_nodes, 2);
}

#[test]
fn split_view_at_same_height_is_divergence() {
    let observations = vec![observed("a", 100, "abc"), observed("b", 100, "def")];
    let verdict = classify(&observations);
    assert_eq!(verdict.class, VerdictClass::Divergence);
}

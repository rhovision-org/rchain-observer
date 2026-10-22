use std::{
    collections::HashSet,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures::future::join_all;
use reqwest::Client;
use serde::Deserialize;

use crate::{
    ancestry::verify_ancestry,
    config::{validate_targets, MAX_ANCESTRY_BLOCKS},
    http::{get_json, SNAPSHOT_BODY_LIMIT},
    model::{
        AncestryCheck, AncestryClass, FinalizedIdentity, HeightSummary, NodeTarget, Observation,
        ObservationReport, VerdictClass,
    },
    verdict::{classify, finalized_identity, refine_with_ancestry},
};

const DEFAULT_ANCESTRY_MAX_BLOCKS: usize = 512;

#[derive(Debug, Deserialize)]
struct StatusResponse {
    #[serde(rename = "networkId")]
    network_id: Option<String>,
    #[serde(rename = "shardId")]
    shard_id: Option<String>,
    #[serde(rename = "latestBlockNumber")]
    latest_block_number: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FinalizedResponse {
    #[serde(rename = "blockInfo")]
    block_info: Option<BlockInfo>,
}

#[derive(Debug, Deserialize)]
struct BlockInfo {
    #[serde(rename = "blockNumber")]
    block_number: Option<u64>,
    #[serde(rename = "blockHash")]
    block_hash: Option<String>,
    #[serde(rename = "shardId")]
    shard_id: Option<String>,
}

#[derive(Clone)]
pub struct Observer {
    client: Client,
    nodes: Vec<NodeTarget>,
    ancestry_max_blocks: usize,
    ancestry_timeout: Duration,
}

impl Observer {
    pub fn new(nodes: Vec<NodeTarget>, timeout: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(30),
            "timeout must be between 1 and 30000 ms"
        );
        let nodes = validate_targets(nodes).map_err(anyhow::Error::msg)?;
        let client = Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            nodes,
            ancestry_max_blocks: DEFAULT_ANCESTRY_MAX_BLOCKS,
            ancestry_timeout: timeout.saturating_mul(2),
        })
    }

    pub fn with_ancestry_max_blocks(mut self, max_blocks: usize) -> Self {
        self.ancestry_max_blocks = max_blocks.min(MAX_ANCESTRY_BLOCKS);
        self
    }

    pub async fn observe(&self) -> ObservationReport {
        let started = Instant::now();
        let observed_at_ms = unix_ms();
        let observations = join_all(
            self.nodes
                .iter()
                .cloned()
                .map(|node| observe_node(self.client.clone(), node)),
        )
        .await;

        let mut verdict = classify(&observations);
        let ancestry_checks = if verdict.class == VerdictClass::HeightSkew {
            self.verify_height_skew(&observations, verdict.reference.as_ref())
                .await
        } else {
            Vec::new()
        };
        verdict = refine_with_ancestry(verdict, &ancestry_checks);

        let heights = summarize_heights(&observations);
        ObservationReport {
            schema_version: 1,
            observed_at_ms,
            completed_at_ms: unix_ms(),
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            heights,
            observations,
            ancestry_checks,
            verdict,
        }
    }

    async fn verify_height_skew(
        &self,
        observations: &[Observation],
        reference: Option<&FinalizedIdentity>,
    ) -> Vec<AncestryCheck> {
        let Some(reference) = reference else {
            return Vec::new();
        };

        let witnesses = observations
            .iter()
            .filter(|observation| finalized_identity(observation).as_ref() == Some(reference))
            .filter_map(|observation| {
                self.nodes
                    .iter()
                    .find(|node| node.name == observation.node)
                    .cloned()
            })
            .collect::<Vec<_>>();

        let mut seen = HashSet::new();
        let mut lower_identities = observations
            .iter()
            .filter_map(finalized_identity)
            .filter(|identity| {
                identity != reference && identity.block_number < reference.block_number
            })
            .filter(|identity| seen.insert(identity.clone()))
            .collect::<Vec<_>>();
        lower_identities.sort_by(|left, right| {
            left.block_number
                .cmp(&right.block_number)
                .then_with(|| left.block_hash.cmp(&right.block_hash))
        });

        let mut checks = Vec::with_capacity(lower_identities.len());
        // All pairs and witness fallbacks share one wall-time budget.
        let deadline = tokio::time::Instant::now() + self.ancestry_timeout;
        for lower in lower_identities {
            if witnesses.is_empty() {
                checks.push(AncestryCheck {
                    class: AncestryClass::UnresolvedHeightSkew,
                    lower,
                    higher: reference.clone(),
                    witness_node: None,
                    examined_blocks: 0,
                    cryptographically_verified_blocks: 0,
                    path: Vec::new(),
                    reason: "no node reporting the highest finalized reference is available as an ancestry witness".to_string(),
                });
                continue;
            }

            let mut fallback = None;
            for witness in &witnesses {
                let check = match tokio::time::timeout_at(
                    deadline,
                    verify_ancestry(
                        &self.client,
                        witness,
                        &lower,
                        reference,
                        self.ancestry_max_blocks,
                    ),
                )
                .await
                {
                    Ok(check) => check,
                    Err(_) => AncestryCheck {
                        class: AncestryClass::UnresolvedHeightSkew,
                        lower: lower.clone(),
                        higher: reference.clone(),
                        witness_node: Some(witness.name.clone()),
                        examined_blocks: 0,
                        cryptographically_verified_blocks: 0,
                        path: Vec::new(),
                        reason: "ancestry verification exceeded the total time budget".to_string(),
                    },
                };

                if check.class != AncestryClass::UnresolvedHeightSkew {
                    fallback = Some(check);
                    break;
                }
                fallback = Some(check);
            }

            if let Some(check) = fallback {
                checks.push(check);
            }
        }

        checks
    }
}

async fn observe_node(client: Client, node: NodeTarget) -> Observation {
    let base = node.base_url.trim_end_matches('/');
    let status_url = format!("{base}/api/status");
    let finalized_url = format!("{base}/api/last-finalized-block");

    let (status, finalized) = tokio::join!(
        get_json::<StatusResponse>(&client, &status_url, SNAPSHOT_BODY_LIMIT),
        get_json::<FinalizedResponse>(&client, &finalized_url, SNAPSHOT_BODY_LIMIT)
    );

    match (status, finalized) {
        (Ok(status), Ok(finalized)) => {
            let block = finalized.block_info;
            let finalized_shard = block.as_ref().and_then(|value| value.shard_id.clone());
            if let (Some(status_shard), Some(block_shard)) = (&status.shard_id, &finalized_shard) {
                if status_shard != block_shard {
                    return failed(
                        &node,
                        "status and finalized block report different shards".to_string(),
                    );
                }
            }
            let mut observation = Observation {
                node: node.name,
                base_url: node.base_url,
                reachable: true,
                network_id: status.network_id,
                shard_id: status.shard_id.or(finalized_shard),
                latest_block: status.latest_block_number,
                finalized_block: block.as_ref().and_then(|value| value.block_number),
                finalized_hash: block.and_then(|value| value.block_hash),
                error: None,
            };
            if finalized_identity(&observation).is_none() {
                observation.error = Some(
                    "node omitted or malformed a required finalized identity field".to_string(),
                );
            } else {
                observation.finalized_hash = observation
                    .finalized_hash
                    .map(|hash| hash.to_ascii_lowercase());
            }
            observation
        }
        (Err(error), _) | (_, Err(error)) => failed(&node, error.to_string()),
    }
}

fn failed(node: &NodeTarget, error: String) -> Observation {
    Observation {
        node: node.name.clone(),
        base_url: node.base_url.clone(),
        reachable: false,
        network_id: None,
        shard_id: None,
        latest_block: None,
        finalized_block: None,
        finalized_hash: None,
        error: Some(error),
    }
}

fn summarize_heights(observations: &[Observation]) -> HeightSummary {
    let latest = observations
        .iter()
        .filter(|value| value.reachable)
        .filter_map(|value| value.latest_block);
    let finalized = observations
        .iter()
        .filter(|value| value.reachable)
        .filter_map(|value| value.finalized_block);
    let latest_min = latest.clone().min();
    let latest_max = latest.max();
    let finalized_min = finalized.clone().min();
    let finalized_max = finalized.max();

    HeightSummary {
        reachable_nodes: observations.iter().filter(|value| value.reachable).count(),
        latest_min,
        latest_max,
        finalized_min,
        finalized_max,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

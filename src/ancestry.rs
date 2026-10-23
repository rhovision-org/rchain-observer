use std::collections::{HashMap, VecDeque};

use reqwest::Client;

use crate::{
    config::MAX_ANCESTRY_BLOCKS,
    model::{AncestryCheck, AncestryClass, FinalizedIdentity, NodeTarget},
    raw_block::fetch_verified_block,
};

#[derive(Debug)]
struct QueueEntry {
    hash: String,
}

pub async fn verify_ancestry(
    client: &Client,
    witness: &NodeTarget,
    lower: &FinalizedIdentity,
    higher: &FinalizedIdentity,
    max_blocks: usize,
) -> AncestryCheck {
    let max_blocks = max_blocks.min(MAX_ANCESTRY_BLOCKS);
    if lower.network_id != higher.network_id || lower.shard_id != higher.shard_id {
        return unresolved(
            witness,
            lower,
            higher,
            0,
            "network or shard identity differs",
        );
    }

    if lower.block_number >= higher.block_number {
        return unresolved(
            witness,
            lower,
            higher,
            0,
            "ancestry verification requires a strictly lower finalized height",
        );
    }

    if max_blocks == 0 {
        return unresolved(witness, lower, higher, 0, "ancestry block budget is zero");
    }

    let mut queue = VecDeque::from([QueueEntry {
        hash: higher.block_hash.clone(),
    }]);
    // One predecessor per unique hash bounds storage linearly in the block budget.
    let mut predecessors = HashMap::from([(higher.block_hash.clone(), None::<String>)]);
    let mut upper_bounds: HashMap<String, u64> = HashMap::new();
    let mut verified_heights: HashMap<String, u64> = HashMap::new();
    let mut examined_blocks = 0usize;

    while let Some(entry) = queue.pop_front() {
        if examined_blocks >= max_blocks {
            return unresolved(
                witness,
                lower,
                higher,
                examined_blocks,
                &format!("ancestry traversal exceeded the {max_blocks}-block evidence budget"),
            );
        }

        let block = match fetch_verified_block(client, witness, &entry.hash).await {
            Ok(block) => block,
            Err(reason) => return unresolved(witness, lower, higher, examined_blocks, &reason),
        };
        examined_blocks += 1;

        if block.block_hash != entry.hash {
            return unresolved(
                witness,
                lower,
                higher,
                examined_blocks,
                "node returned a block hash different from the requested content address",
            );
        }

        if block.shard_id != higher.shard_id {
            return unresolved(
                witness,
                lower,
                higher,
                examined_blocks,
                "block evidence changed or omitted the expected shard identity",
            );
        }

        if entry.hash == higher.block_hash && block.block_number != higher.block_number {
            return unresolved(
                witness,
                lower,
                higher,
                examined_blocks,
                "highest finalized block was returned with an unexpected height",
            );
        }

        if let Some(parent_height) = upper_bounds.get(&entry.hash) {
            if block.block_number >= *parent_height {
                return unresolved(
                    witness,
                    lower,
                    higher,
                    examined_blocks,
                    "justification edge did not descend in block height",
                );
            }
        }
        verified_heights.insert(entry.hash.clone(), block.block_number);

        if block.block_hash == lower.block_hash {
            if block.block_number != lower.block_number {
                return unresolved(
                    witness,
                    lower,
                    higher,
                    examined_blocks,
                    "target block hash was returned with an unexpected height",
                );
            }
            let mut path = vec![entry.hash.clone()];
            let mut cursor = entry.hash.as_str();
            while let Some(Some(parent)) = predecessors.get(cursor) {
                path.push(parent.clone());
                cursor = parent;
            }
            path.reverse();
            return AncestryCheck {
                class: AncestryClass::ProvenLag,
                lower: lower.clone(),
                higher: higher.clone(),
                witness_node: Some(witness.name.clone()),
                examined_blocks,
                cryptographically_verified_blocks: examined_blocks,
                path,
                reason: "lower finalized block is reachable through a justification DAG decoded only from BLAKE2b-256-verified raw block preimages".to_string(),
            };
        }

        if block.block_number < lower.block_number {
            continue;
        }

        if block.block_number == lower.block_number {
            continue;
        }

        for justification in block.justifications {
            if verified_heights
                .get(&justification)
                .is_some_and(|height| *height >= block.block_number)
            {
                return unresolved(
                    witness,
                    lower,
                    higher,
                    examined_blocks,
                    "justification edge did not descend in block height",
                );
            }
            upper_bounds
                .entry(justification.clone())
                .and_modify(|bound| *bound = (*bound).min(block.block_number))
                .or_insert(block.block_number);
            if predecessors.contains_key(&justification) {
                continue;
            }
            if predecessors.len() >= max_blocks {
                return unresolved(
                    witness,
                    lower,
                    higher,
                    examined_blocks,
                    "ancestry frontier exceeded the unique-block evidence budget",
                );
            }
            predecessors.insert(justification.clone(), Some(entry.hash.clone()));
            queue.push_back(QueueEntry {
                hash: justification,
            });
        }
    }

    AncestryCheck {
        class: AncestryClass::ConflictingFinalizedHistory,
        lower: lower.clone(),
        higher: higher.clone(),
        witness_node: Some(witness.name.clone()),
        examined_blocks,
        cryptographically_verified_blocks: examined_blocks,
        path: Vec::new(),
        reason: "complete cryptographically verified justification traversal reached the target height without the lower finalized hash".to_string(),
    }
}

fn unresolved(
    witness: &NodeTarget,
    lower: &FinalizedIdentity,
    higher: &FinalizedIdentity,
    examined_blocks: usize,
    reason: &str,
) -> AncestryCheck {
    AncestryCheck {
        class: AncestryClass::UnresolvedHeightSkew,
        lower: lower.clone(),
        higher: higher.clone(),
        witness_node: Some(witness.name.clone()),
        examined_blocks,
        cryptographically_verified_blocks: examined_blocks,
        path: Vec::new(),
        reason: reason.to_string(),
    }
}

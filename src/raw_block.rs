use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{Signature, VerifyingKey};
use prost::Message;
use reqwest::Client;
use serde::Deserialize;

use crate::http::{get_json, RAW_BODY_LIMIT};
use crate::model::NodeTarget;

#[derive(Debug, Deserialize)]
struct RawBlockResponse {
    #[serde(rename = "blockHash")]
    block_hash: String,
    encoded: String,
    sender: String,
    #[serde(rename = "sigAlgorithm")]
    sig_algorithm: String,
    sig: String,
}

/// Minimal protobuf view of `BlockMessageProto`.
///
/// Unknown fields are deliberately ignored by prost. The observer only needs the fields that define
/// ancestry plus the sender/signature-algorithm fields used to cross-check the raw envelope. The raw
/// bytes themselves are hashed before any decoded field is trusted.
#[derive(Clone, PartialEq, Message)]
struct BlockMessageProof {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBlockEvidence {
    pub block_hash: String,
    pub block_number: u64,
    pub shard_id: String,
    pub justifications: Vec<String>,
}

pub async fn fetch_verified_block(
    client: &Client,
    witness: &NodeTarget,
    requested_hash: &str,
) -> Result<VerifiedBlockEvidence, String> {
    decode_hash("requested block hash", requested_hash)?;
    let base = witness.base_url.trim_end_matches('/');
    let url = format!("{base}/api/v1/block/{requested_hash}/raw");
    let raw = get_json::<RawBlockResponse>(client, &url, RAW_BODY_LIMIT).await?;
    verify_raw_block(requested_hash, raw)
}

fn verify_raw_block(
    requested_hash: &str,
    raw: RawBlockResponse,
) -> Result<VerifiedBlockEvidence, String> {
    let requested = decode_hash("requested block hash", requested_hash)?;
    let claimed = decode_hash("raw response blockHash", &raw.block_hash)?;
    if requested != claimed {
        return Err(
            "raw response blockHash differs from the requested content address".to_string(),
        );
    }

    let encoded = hex::decode(&raw.encoded)
        .map_err(|_| "raw response encoded field is not valid hexadecimal".to_string())?;
    if encoded.is_empty() {
        return Err("raw response encoded preimage is empty".to_string());
    }

    let mut hasher =
        Blake2bVar::new(32).map_err(|_| "failed to initialize BLAKE2b-256 verifier".to_string())?;
    hasher.update(&encoded);
    let mut digest = [0u8; 32];
    hasher
        .finalize_variable(&mut digest)
        .map_err(|_| "failed to finalize BLAKE2b-256 verifier".to_string())?;
    if digest != requested.as_slice() {
        return Err(
            "BLAKE2b-256(raw encoded preimage) does not match the requested block hash".to_string(),
        );
    }

    let decoded = BlockMessageProof::decode(encoded.as_slice()).map_err(|error| {
        format!("verified raw block preimage is not a valid BlockMessageProto: {error}")
    })?;

    // `to_bytes_cleared()` maps the fixed-width block hash to 32 zero bytes and clears `sig`.
    // The zero-filled hash is intentionally still encoded because the Rust model cannot represent
    // an empty fixed-width `BlockHash`.
    if decoded.block_hash != vec![0u8; 32] || !decoded.sig.is_empty() {
        return Err(
            "raw block preimage did not contain the expected cleared blockHash/sig fields"
                .to_string(),
        );
    }

    let outer_sender = hex::decode(&raw.sender)
        .map_err(|_| "raw response sender is not valid hexadecimal".to_string())?;
    if decoded.sender != outer_sender {
        return Err(
            "raw response sender differs from the sender committed by the verified preimage"
                .to_string(),
        );
    }
    if decoded.sig_algorithm != raw.sig_algorithm {
        return Err(
            "raw response sigAlgorithm differs from the algorithm committed by the verified preimage"
                .to_string(),
        );
    }
    let detached_sig = hex::decode(&raw.sig)
        .map_err(|_| "raw response sig is not valid hexadecimal".to_string())?;
    verify_detached_signature(&requested, &outer_sender, &raw.sig_algorithm, &detached_sig)?;

    let block_number = u64::try_from(decoded.block_number)
        .map_err(|_| "verified raw block contains a negative blockNumber".to_string())?;
    if decoded.shard_id.is_empty() {
        return Err("verified raw block preimage omitted shardId".to_string());
    }

    let mut justifications = Vec::with_capacity(decoded.justifications.len());
    if decoded.justifications.len() > 1024 {
        return Err("verified raw block exceeds the 1024-justification evidence limit".to_string());
    }
    for justification in decoded.justifications {
        if justification.len() != 32 {
            return Err(format!(
                "verified raw block contains a justification with {} bytes instead of 32",
                justification.len()
            ));
        }
        justifications.push(hex::encode(justification));
    }

    Ok(VerifiedBlockEvidence {
        block_hash: hex::encode(requested),
        block_number,
        shard_id: decoded.shard_id,
        justifications,
    })
}

fn verify_detached_signature(
    block_hash: &[u8],
    sender: &[u8],
    algorithm: &str,
    signature: &[u8],
) -> Result<(), String> {
    if block_hash.len() != 32 {
        return Err("block signature verifier requires a 32-byte block hash".to_string());
    }

    let public_key = k256::PublicKey::from_sec1_bytes(sender)
        .map_err(|_| "raw block sender is not a valid secp256k1 public key".to_string())?;
    let verifying_key = VerifyingKey::from(&public_key);

    let parsed = match algorithm.to_ascii_lowercase().as_str() {
        "secp256k1" => Signature::from_der(signature)
            .map_err(|_| "raw block signature is not valid DER secp256k1".to_string())?,
        "secp256k1:eth" => Signature::from_slice(signature).map_err(|_| {
            "raw block signature is not a valid 64-byte secp256k1:eth signature".to_string()
        })?,
        _ => return Err("raw block uses an unsupported signature algorithm".to_string()),
    };

    verifying_key
        .verify_prehash(block_hash, &parsed)
        .map_err(|_| "raw block proposer signature does not verify over blockHash".to_string())
}

fn decode_hash(label: &str, value: &str) -> Result<Vec<u8>, String> {
    let bytes = hex::decode(value).map_err(|_| format!("{label} is not valid hexadecimal"))?;
    if bytes.len() != 32 {
        return Err(format!("{label} must be exactly 32 bytes"));
    }
    Ok(bytes)
}

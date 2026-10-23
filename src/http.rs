use reqwest::{Client, Response};
use serde::de::DeserializeOwned;

pub const SNAPSHOT_BODY_LIMIT: usize = 64 * 1024;
pub const RAW_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Read the body incrementally: Content-Length alone cannot bound chunked responses.
/// Public failures intentionally omit URLs, addresses and upstream response content.
pub async fn get_json<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    limit: usize,
) -> Result<T, String> {
    let response = client.get(url).send().await.map_err(transport_error)?;
    read_json(response, limit).await
}

async fn read_json<T: DeserializeOwned>(mut response: Response, limit: usize) -> Result<T, String> {
    if !response.status().is_success() {
        return Err(format!(
            "upstream returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err("upstream response exceeded the body limit".to_string());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err("upstream response exceeded the body limit".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| "upstream returned invalid or incompatible JSON".to_string())
}

fn transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "upstream request timed out"
    } else if error.is_connect() {
        "upstream connection failed"
    } else {
        "upstream transport failed"
    }
    .to_string()
}

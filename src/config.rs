use std::collections::HashSet;

use crate::model::NodeTarget;

pub const MAX_NODES: usize = 32;
pub const MAX_ANCESTRY_BLOCKS: usize = 4096;

pub fn parse_node(value: &str) -> Result<NodeTarget, String> {
    let (name, base_url) = value.split_once('=').ok_or("node must be NAME=URL")?;
    validate_target(NodeTarget {
        name: name.to_string(),
        base_url: base_url.to_string(),
    })
}

fn validate_target(mut node: NodeTarget) -> Result<NodeTarget, String> {
    if node.name.is_empty()
        || node.name.len() > 64
        || !node
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(
            "node name must contain 1..64 letters, digits, dots, underscores or hyphens"
                .to_string(),
        );
    }
    let url = reqwest::Url::parse(&node.base_url).map_err(|_| "node URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("node URL must be an absolute HTTP or HTTPS URL".to_string());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("node URL must not contain credentials, a query or a fragment".to_string());
    }
    node.base_url = url.as_str().trim_end_matches('/').to_string();
    Ok(node)
}

pub fn validate_targets(nodes: Vec<NodeTarget>) -> Result<Vec<NodeTarget>, String> {
    if nodes.is_empty() || nodes.len() > MAX_NODES {
        return Err(format!("configure between 1 and {MAX_NODES} nodes"));
    }
    let mut names = HashSet::new();
    let mut urls = HashSet::new();
    nodes
        .into_iter()
        .map(|node| {
            let node = validate_target(node)?;
            if !names.insert(node.name.clone()) || !urls.insert(node.base_url.clone()) {
                return Err("node names and normalized URLs must be unique".to_string());
            }
            Ok(node)
        })
        .collect()
}

use std::{
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use rchain_observer::{
    config::{parse_node, MAX_ANCESTRY_BLOCKS},
    model::NodeTarget,
    observer::Observer,
    service::{self, ApiState, CachedReport},
};
use tokio::{
    net::TcpListener,
    sync::RwLock,
    time::{self, MissedTickBehavior},
};

#[derive(Debug, Parser)]
#[command(name = "rchain-observer")]
#[command(about = "Cross-check finalized state across RChain nodes")]
struct Cli {
    #[arg(long = "node", value_parser = parse_node, required = true)]
    nodes: Vec<NodeTarget>,

    #[arg(long, default_value_t = 3000)]
    timeout_ms: u64,

    #[arg(long, default_value_t = 512)]
    ancestry_max_blocks: usize,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Once,
    Serve {
        #[arg(long, default_value = "127.0.0.1:8088")]
        listen: String,
        #[arg(long, default_value_t = 5000)]
        refresh_ms: u64,
        #[arg(long, default_value_t = 30000)]
        stale_after_ms: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.ancestry_max_blocks == 0 || cli.ancestry_max_blocks > MAX_ANCESTRY_BLOCKS {
        return Err(anyhow!("ancestry-max-blocks must be between 1 and 4096"));
    }
    let observer = Observer::new(cli.nodes, Duration::from_millis(cli.timeout_ms))?
        .with_ancestry_max_blocks(cli.ancestry_max_blocks);

    match cli.command {
        Command::Once => {
            let report = observer.observe().await;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Serve {
            listen,
            refresh_ms,
            stale_after_ms,
        } => serve(observer, &listen, refresh_ms, stale_after_ms).await?,
    }

    Ok(())
}

async fn serve(
    observer: Observer,
    listen: &str,
    refresh_ms: u64,
    stale_after_ms: u64,
) -> anyhow::Result<()> {
    if refresh_ms < 1000 {
        return Err(anyhow!("refresh-ms must be at least 1000"));
    }

    if stale_after_ms < 1000 {
        return Err(anyhow!("stale-after-ms must be at least 1000"));
    }

    let address = SocketAddr::from_str(listen)
        .with_context(|| format!("invalid listen address: {listen}"))?;
    let sampled = Instant::now();
    let initial = observer.observe().await;
    let state = ApiState {
        snapshot: Arc::new(RwLock::new(CachedReport {
            report: initial,
            sampled,
        })),
        stale_after: Duration::from_millis(stale_after_ms),
    };

    let refresh_state = state.clone();
    let refresh_observer = observer.clone();
    let refresh_task = tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_millis(refresh_ms));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            let sampled = Instant::now();
            let report = refresh_observer.observe().await;
            *refresh_state.snapshot.write().await = CachedReport { report, sampled };
        }
    });

    let app = service::router(state);
    let listener = TcpListener::bind(address).await?;
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await;
    refresh_task.abort();
    result.map_err(|error| anyhow!(error))?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

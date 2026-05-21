// SPDX-License-Identifier: Apache-2.0

mod live;
mod plot;

use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use efbench::proto::{Ping, benchmark_client::BenchmarkClient};
use tokio::sync::mpsc;

#[derive(Debug, Default)]
pub(crate) struct BenchState {
    total_latency_us: AtomicU64,
    req_count: AtomicU64,
}

#[derive(Clone, Copy)]
pub(crate) struct Metrics {
    pub(crate) avg_latency_us: f64,
    pub(crate) req_count: u64,
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes: u64,
}

#[derive(Parser)]
#[command(name = "efbench-client", about = "Benchmark client for efense")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Live TUI benchmark
    Live {
        #[arg(long)]
        iface: String,
        #[arg(long)]
        ip: String,
        #[arg(long)]
        port: u16,
        #[arg(long, default_value = "benchmark_output")]
        output: String,
    },
    /// Run benchmark and save results
    Plot {
        #[arg(long)]
        iface: String,
        #[arg(long)]
        ip: String,
        #[arg(long)]
        port: u16,
        #[arg(long, default_value = "benchmark_output")]
        output: String,
    },
}

pub(crate) fn check_interface(iface: &str) -> anyhow::Result<()> {
    let path = format!("/sys/class/net/{iface}");
    if !std::path::Path::new(&path).is_dir() {
        anyhow::bail!("network interface '{iface}' not found at {path}");
    }
    Ok(())
}

pub(crate) async fn check_connection(server_addr: &str) -> anyhow::Result<()> {
    BenchmarkClient::connect(format!("http://{server_addr}"))
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to connect to server at {server_addr}: {e}")
        })?;
    Ok(())
}

pub(crate) fn read_net_stat(iface: &str, stat: &str) -> u64 {
    let path = format!("/sys/class/net/{iface}/statistics/{stat}");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

pub(crate) async fn benchmark_loop(
    server_addr: String,
    _iface: String,
    state: Arc<BenchState>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client =
        BenchmarkClient::connect(format!("http://{server_addr}")).await?;

    let (req_tx, req_rx) = mpsc::channel::<Ping>(1000);
    let outbound = tokio_stream::wrappers::ReceiverStream::new(req_rx);

    let response = client.ping_pong(tonic::Request::new(outbound)).await?;
    let mut inbound = response.into_inner();

    let mut seq = 0u64;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let sent = Instant::now();
        req_tx.send(Ping { seq, sent_ns: 0 }).await?;

        match inbound.message().await {
            Ok(Some(_)) => {
                let latency = sent.elapsed().as_micros() as u64;
                state.total_latency_us.fetch_add(latency, Ordering::Relaxed);
                state.req_count.fetch_add(1, Ordering::Relaxed);
                seq += 1;
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("gRPC error: {e}");
                break;
            }
        }
    }

    Ok(())
}

pub(crate) async fn reporter_task(
    iface: String,
    state: Arc<BenchState>,
    metrics_tx: mpsc::Sender<Metrics>,
    stop: Arc<AtomicBool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut last_rx = read_net_stat(&iface, "rx_bytes");
    let mut last_tx = read_net_stat(&iface, "tx_bytes");

    loop {
        interval.tick().await;

        if stop.load(Ordering::Relaxed) {
            break;
        }

        let now_rx = read_net_stat(&iface, "rx_bytes");
        let now_tx = read_net_stat(&iface, "tx_bytes");

        let (avg_latency_us, req_count) = {
            let count = state.req_count.load(Ordering::Relaxed);
            let total_latency = state.total_latency_us.load(Ordering::Relaxed);
            let avg = if count > 0 {
                total_latency as f64 / count as f64
            } else {
                0.0
            };
            state.total_latency_us.store(0, Ordering::Relaxed);
            state.req_count.store(0, Ordering::Relaxed);
            (avg, count)
        };

        let _ = metrics_tx
            .send(Metrics {
                avg_latency_us,
                req_count,
                rx_bytes: now_rx.saturating_sub(last_rx),
                tx_bytes: now_tx.saturating_sub(last_tx),
            })
            .await;

        last_rx = now_rx;
        last_tx = now_tx;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Live {
            iface,
            ip,
            port,
            output,
        } => live::run_live(iface, ip, port, output).await,
        Command::Plot {
            iface,
            ip,
            port,
            output,
        } => plot::run_plot(iface, ip, port, output).await,
    }
}

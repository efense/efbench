// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::mpsc;

use crate::Metrics;

pub async fn run_plot(
    iface: String,
    ip: String,
    port: u16,
    output: String,
) -> anyhow::Result<()> {
    crate::check_interface(&iface)?;
    let server_addr = format!("{ip}:{port}");
    crate::check_connection(&server_addr).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(crate::BenchState::default());

    let (metrics_tx, mut metrics_rx) = mpsc::channel::<Metrics>(4096);

    let b_state = state.clone();
    let b_stop = stop.clone();
    let b_addr = server_addr.clone();
    let bench_handle = tokio::spawn(async move {
        if let Err(e) = crate::benchmark_loop(b_addr, b_state, b_stop).await {
            eprintln!("Benchmark error: {e}");
        }
    });

    let r_state = state.clone();
    let r_stop = stop.clone();
    let r_iface = iface.clone();
    let r_tx = metrics_tx.clone();
    let report_handle = tokio::spawn(async move {
        crate::reporter_task(r_iface, r_state, r_tx, r_stop).await;
    });

    let mut all_metrics: Vec<Metrics> = Vec::new();
    let ctrlc_stop = stop.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        ctrlc_stop.store(true, Ordering::Relaxed);
    });

    println!("Benchmark running... Press Ctrl-C to stop.");

    loop {
        tokio::select! {
            Some(m) = metrics_rx.recv() => {
                all_metrics.push(m);
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if stop.load(Ordering::Relaxed) {
                    while let Ok(m) = metrics_rx.try_recv() {
                        all_metrics.push(m);
                    }
                    break;
                }
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = bench_handle.await;
    let _ = report_handle.await;

    save_plot(&all_metrics, &output)?;
    Ok(())
}

pub fn save_plot(metrics: &[Metrics], output: &str) -> anyhow::Result<()> {
    use plotters::prelude::*;

    let path = format!("{output}.png");
    let root = BitMapBackend::new(&path, (1920, 1080)).into_drawing_area();
    root.fill(&WHITE)?;

    let areas = root.split_evenly((2, 2));

    // --- Latency ---
    let latency_vals: Vec<f64> =
        metrics.iter().map(|m| m.avg_latency_us).collect();
    let max_lat = latency_vals
        .iter()
        .cloned()
        .fold(0.0_f64, f64::max)
        .max(1.0);
    {
        let mut chart = ChartBuilder::on(&areas[0])
            .caption("Latency (μs)", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(20)
            .y_label_area_size(30)
            .build_cartesian_2d(0..metrics.len(), 0.0..max_lat * 1.1)?;
        chart.configure_mesh().draw()?;
        chart
            .draw_series(LineSeries::new(
                latency_vals.iter().enumerate().map(|(i, &v)| (i, v)),
                &CYAN,
            ))?
            .label("latency")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], CYAN));
        chart.configure_series_labels().draw()?;
    }

    // --- Throughput ---
    let tp_vals: Vec<f64> =
        metrics.iter().map(|m| m.req_count as f64).collect();
    let max_tp = tp_vals.iter().cloned().fold(0.0_f64, f64::max).max(1.0);
    {
        let mut chart = ChartBuilder::on(&areas[1])
            .caption("Throughput (req/s)", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(20)
            .y_label_area_size(30)
            .build_cartesian_2d(0..metrics.len(), 0.0..max_tp * 1.1)?;
        chart.configure_mesh().draw()?;
        chart
            .draw_series(LineSeries::new(
                tp_vals.iter().enumerate().map(|(i, &v)| (i, v)),
                &GREEN,
            ))?
            .label("throughput")
            .legend(|(x, y)| {
                PathElement::new(vec![(x, y), (x + 20, y)], GREEN)
            });
        chart.configure_series_labels().draw()?;
    }

    // --- RX ---
    let rx_vals: Vec<f64> = metrics
        .iter()
        .map(|m| m.rx_bytes as f64 / (1024.0 * 1024.0))
        .collect();
    let max_rx = rx_vals.iter().cloned().fold(0.0_f64, f64::max).max(1.0);
    {
        let mut chart = ChartBuilder::on(&areas[2])
            .caption("RX (MiB/s)", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(20)
            .y_label_area_size(30)
            .build_cartesian_2d(0..metrics.len(), 0.0..max_rx * 1.1)?;
        chart.configure_mesh().draw()?;
        chart
            .draw_series(LineSeries::new(
                rx_vals.iter().enumerate().map(|(i, &v)| (i, v)),
                &YELLOW,
            ))?
            .label("RX")
            .legend(|(x, y)| {
                PathElement::new(vec![(x, y), (x + 20, y)], YELLOW)
            });
        chart.configure_series_labels().draw()?;
    }

    // --- TX ---
    let tx_vals: Vec<f64> = metrics
        .iter()
        .map(|m| m.tx_bytes as f64 / (1024.0 * 1024.0))
        .collect();
    let max_tx = tx_vals.iter().cloned().fold(0.0_f64, f64::max).max(1.0);
    {
        let mut chart = ChartBuilder::on(&areas[3])
            .caption("TX (MiB/s)", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(20)
            .y_label_area_size(30)
            .build_cartesian_2d(0..metrics.len(), 0.0..max_tx * 1.1)?;
        chart.configure_mesh().draw()?;
        chart
            .draw_series(LineSeries::new(
                tx_vals.iter().enumerate().map(|(i, &v)| (i, v)),
                &MAGENTA,
            ))?
            .label("TX")
            .legend(|(x, y)| {
                PathElement::new(vec![(x, y), (x + 20, y)], MAGENTA)
            });
        chart.configure_series_labels().draw()?;
    }

    root.present()?;
    println!("Plot saved to {path}");
    Ok(())
}

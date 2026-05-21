// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, Borders, Paragraph,
        canvas::{Canvas, Context, Line as CanvasLine},
    },
};
use tokio::sync::mpsc;

use crate::Metrics;

const MAX_POINTS: usize = 120;

#[derive(Clone)]
pub struct AppData {
    pub ip: String,
    pub port: u16,
    latencies: VecDeque<f64>,
    throughputs: VecDeque<f64>,
    rx_rates: VecDeque<f64>,
    tx_rates: VecDeque<f64>,
    pub current_latency: f64,
    pub current_throughput: u64,
    pub current_rx: u64,
    pub current_tx: u64,
    pub total_reqs: u64,
    pub elapsed: u64,
    pub connected: bool,
}

impl AppData {
    pub fn new(ip: String, port: u16) -> Self {
        Self {
            ip,
            port,
            latencies: VecDeque::with_capacity(MAX_POINTS),
            throughputs: VecDeque::with_capacity(MAX_POINTS),
            rx_rates: VecDeque::with_capacity(MAX_POINTS),
            tx_rates: VecDeque::with_capacity(MAX_POINTS),
            current_latency: 0.0,
            current_throughput: 0,
            current_rx: 0,
            current_tx: 0,
            total_reqs: 0,
            elapsed: 0,
            connected: false,
        }
    }

    pub fn push_metrics(&mut self, m: Metrics) {
        self.current_latency = m.avg_latency_us;
        self.current_throughput = m.req_count;
        self.current_rx = m.rx_bytes;
        self.current_tx = m.tx_bytes;

        if self.latencies.len() >= MAX_POINTS {
            self.latencies.pop_front();
        }
        self.latencies.push_back(self.current_latency);

        if self.throughputs.len() >= MAX_POINTS {
            self.throughputs.pop_front();
        }
        self.throughputs.push_back(self.current_throughput as f64);

        if self.rx_rates.len() >= MAX_POINTS {
            self.rx_rates.pop_front();
        }
        self.rx_rates.push_back(m.rx_bytes as f64);

        if self.tx_rates.len() >= MAX_POINTS {
            self.tx_rates.pop_front();
        }
        self.tx_rates.push_back(m.tx_bytes as f64);

        self.total_reqs += m.req_count;
        self.elapsed += 1;
    }
}

fn make_canvas<'a>(
    title: &'a str,
    data: Vec<(f64, f64)>,
    y_max: f64,
    color: Color,
) -> Canvas<'a, impl Fn(&mut Context)> {
    let y_max = if y_max <= 0.0 { 1.0 } else { y_max * 1.1 };

    Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_alignment(Alignment::Center),
        )
        .x_bounds([0.0, MAX_POINTS as f64])
        .y_bounds([0.0, y_max])
        .marker(Marker::Braille)
        .paint(move |ctx| {
            for pair in data.windows(2) {
                ctx.draw(&CanvasLine {
                    x1: pair[0].0,
                    y1: pair[0].1,
                    x2: pair[1].0,
                    y2: pair[1].1,
                    color,
                });
            }
        })
}

fn draw_ui(frame: &mut ratatui::Frame, app: &AppData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let header = format!(
        " {}:{}  |  Latency: {:.0}μs  |  Throughput: {} req/s  |  RX: {:.1} \
         MiB/s  TX: {:.1} MiB/s",
        app.ip,
        app.port,
        app.current_latency,
        app.current_throughput,
        app.current_rx as f64 / (1024.0 * 1024.0),
        app.current_tx as f64 / (1024.0 * 1024.0),
    );

    let header_block = Paragraph::new(Line::from(Span::styled(
        &header,
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" efbench - Live Benchmark ")
            .title_alignment(Alignment::Center),
    );
    frame.render_widget(header_block, chunks[0]);

    let chart_area = chunks[1];
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(chart_area);
    let [left, right] = [cols[0], cols[1]];
    let rows_left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(left);
    let rows_right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(right);

    let latency_data: Vec<(f64, f64)> = app
        .latencies
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v))
        .collect();
    let max_latency = app.latencies.iter().cloned().fold(0.0_f64, f64::max);
    let latency_title =
        format!("Latency (μs)  [cur: {:.0}]", app.current_latency);
    frame.render_widget(
        make_canvas(&latency_title, latency_data, max_latency, Color::Cyan),
        rows_left[0],
    );

    let tp_data: Vec<(f64, f64)> = app
        .throughputs
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v))
        .collect();
    let max_tp = app.throughputs.iter().cloned().fold(0.0_f64, f64::max);
    let tp_title =
        format!("Throughput (req/s)  [cur: {}]", app.current_throughput);
    frame.render_widget(
        make_canvas(&tp_title, tp_data, max_tp, Color::Green),
        rows_right[0],
    );

    let rx_data: Vec<(f64, f64)> = app
        .rx_rates
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v / (1024.0 * 1024.0)))
        .collect();
    let max_rx = rx_data.iter().map(|&(_, v)| v).fold(0.0_f64, f64::max);
    let rx_title = format!(
        "RX (MiB/s)  [cur: {:.1}]",
        app.current_rx as f64 / (1024.0 * 1024.0)
    );
    frame.render_widget(
        make_canvas(&rx_title, rx_data, max_rx, Color::Yellow),
        rows_left[1],
    );

    let tx_data: Vec<(f64, f64)> = app
        .tx_rates
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v / (1024.0 * 1024.0)))
        .collect();
    let max_tx = tx_data.iter().map(|&(_, v)| v).fold(0.0_f64, f64::max);
    let tx_title = format!(
        "TX (MiB/s)  [cur: {:.1}]",
        app.current_tx as f64 / (1024.0 * 1024.0)
    );
    frame.render_widget(
        make_canvas(&tx_title, tx_data, max_tx, Color::Magenta),
        rows_right[1],
    );

    let footer = Paragraph::new(Line::from(" Press 'q' to quit "))
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default());
    frame.render_widget(footer, chunks[2]);
}

pub fn run_tui(
    metrics_rx: mpsc::Receiver<Metrics>,
    app_data: Arc<Mutex<AppData>>,
    all_metrics: Arc<Mutex<Vec<Metrics>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut rx = metrics_rx;

    loop {
        // Check for key press (non-blocking)
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('q')
        {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            break;
        }

        // Drain available metrics
        while let Ok(m) = rx.try_recv() {
            if let Ok(mut app) = app_data.lock() {
                app.push_metrics(m);
                app.connected = true;
            }
            if let Ok(mut all) = all_metrics.lock() {
                all.push(m);
            }
        }

        // Draw UI
        let app = app_data.lock().unwrap().clone();
        terminal.draw(|f| draw_ui(f, &app))?;

        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
    }

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    Ok(())
}

pub async fn run_live(
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
    let app_data = Arc::new(Mutex::new(AppData::new(ip.clone(), port)));
    let all_metrics: Arc<Mutex<Vec<Metrics>>> =
        Arc::new(Mutex::new(Vec::new()));

    let (metrics_tx, metrics_rx) = mpsc::channel::<Metrics>(16);

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

    let t_app_data = app_data.clone();
    let t_all_metrics = all_metrics.clone();
    let t_stop = stop.clone();
    let t_handle = tokio::task::spawn_blocking(move || {
        run_tui(metrics_rx, t_app_data, t_all_metrics, t_stop)
    });

    t_handle.await??;

    stop.store(true, Ordering::Relaxed);
    let _ = bench_handle.await;
    let _ = report_handle.await;

    let metrics = all_metrics.lock().unwrap().clone();
    crate::plot::save_plot(&metrics, &output)?;

    Ok(())
}

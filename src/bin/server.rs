use std::{collections::HashMap, sync::{Arc, Mutex}, time::Duration, io};
use tokio::time::interval;
use tonic::{transport::Server, Request, Response, Status};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, List, ListItem, Chart, Dataset, GraphType, Axis, Tabs},
    Terminal,
    text::{Line, Span},
    symbols,
};
use crossterm::{
    event::{self, Event, KeyCode, MouseEvent, MouseEventKind, EnableMouseCapture, DisableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dotenvy::dotenv;
use sqlx::postgres::PgPoolOptions;

pub mod monitor {
    tonic::include_proto!("monitor");
}

use monitor::metrics_collector_server::{MetricsCollector, MetricsCollectorServer};
use monitor::{MetricsRequest, MetricsResponse};

const NEON_PALETTE: &[Color] = &[
    Color::Rgb(173, 255, 47),
    Color::Rgb(255, 0, 255),
    Color::Rgb(0, 255, 255),
    Color::Rgb(157, 0, 255),
];

const REALTIME_MAX_POINTS: usize = 60;

// ── Shared data (пишут воркеры, читает TUI-цикл) ────────────────────────────

type SharedState     = Arc<Mutex<HashMap<String, MetricsRequest>>>;
type RealtimeHistory = Arc<Mutex<HashMap<String, Vec<(f64, f64)>>>>;
type MinuteHistory   = Arc<Mutex<HashMap<String, Vec<(f64, f64)>>>>;

// ── Готовые к отрисовке данные — пересчитываются раз в 200 мс ───────────────

/// Одна строка для вкладки «Hosts»
struct HostRow {
    color: Color,
    hostname: String,
    memory_gb: f32,
    cores: Vec<(u32, f32)>,
}

/// Данные одного хоста для графиков — точки уже нормализованы
struct ChartSeries {
    color: Color,
    label: String,
    points: Vec<(f64, f64)>,
}

/// Всё, что нужно draw-замыканию — только читать.
/// Никаких Mutex-lock'ов и аллокаций внутри draw.
struct RenderState {
    hosts: Vec<HostRow>,
    realtime: Vec<ChartSeries>,
    realtime_max_x: f64,
    minute: Vec<ChartSeries>,
    minute_max_x: f64,
}

impl RenderState {
    fn empty() -> Self {
        Self {
            hosts: Vec::new(),
            realtime: Vec::new(),
            realtime_max_x: REALTIME_MAX_POINTS as f64,
            minute: Vec::new(),
            minute_max_x: 1.0,
        }
    }

    /// Берём lock'и один раз, готовим данные, освобождаем.
    fn refresh(
        state: &SharedState,
        realtime_history: &RealtimeHistory,
        minute_history: &MinuteHistory,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        // ── Hosts ────────────────────────────────────────────────────────────
        let hosts = {
            let data = state.lock().unwrap();
            let mut sorted: Vec<_> = data.values().collect();
            sorted.sort_by(|a, b| a.hostname.cmp(&b.hostname));
            sorted.iter().enumerate().map(|(i, m)| HostRow {
                color: NEON_PALETTE[i % NEON_PALETTE.len()],
                hostname: m.hostname.clone(),
                memory_gb: m.memory_usage,
                cores: m.cores.iter().map(|c| (c.core_id, c.usage)).collect(),
            }).collect()
        }; // lock освобождён

        // ── Real-time chart ──────────────────────────────────────────────────
        let (realtime, realtime_max_x) = {
            let history = realtime_history.lock().unwrap();
            let mut sorted_hosts: Vec<_> = history.keys().cloned().collect();
            sorted_hosts.sort();

            let origin = now - REALTIME_MAX_POINTS as f64;

            let series = sorted_hosts.iter().enumerate().map(|(i, host)| {
                let points = history.get(host).map(|pts| {
                    pts.iter().map(|(t, v)| (t - origin, *v)).collect()
                }).unwrap_or_default();

                ChartSeries {
                    color: NEON_PALETTE[i % NEON_PALETTE.len()],
                    label: host.clone(),
                    points,
                }
            }).collect();

            (series, REALTIME_MAX_POINTS as f64)
        }; // lock освобождён

        // ── Minute chart ─────────────────────────────────────────────────────
        let (minute, minute_max_x) = {
            let history = minute_history.lock().unwrap();
            let mut sorted_hosts: Vec<_> = history.keys().cloned().collect();
            sorted_hosts.sort();

            let mut max_x = 1.0_f64;

            let series = sorted_hosts.iter().enumerate().map(|(i, host)| {
                let points: Vec<(f64, f64)> = history.get(host).map(|pts| {
                    pts.iter().enumerate().map(|(idx, (_, v))| (idx as f64, *v)).collect()
                }).unwrap_or_default();

                if let Some(&(x, _)) = points.last() {
                    if x > max_x { max_x = x; }
                }

                ChartSeries {
                    color: NEON_PALETTE[i % NEON_PALETTE.len()],
                    label: host.clone(),
                    points,
                }
            }).collect();

            (series, max_x)
        }; // lock освобождён

        Self { hosts, realtime, realtime_max_x, minute, minute_max_x }
    }
}

// ── gRPC коллектор ───────────────────────────────────────────────────────────

pub struct MyCollector {
    state: SharedState,
    realtime_history: RealtimeHistory,
}

#[tonic::async_trait]
impl MetricsCollector for MyCollector {
    async fn send_metrics(
        &self,
        request: Request<MetricsRequest>,
    ) -> Result<Response<MetricsResponse>, Status> {
        let req = request.into_inner();

        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();

            let avg_cpu = if req.cores.is_empty() { 0.0 } else {
                req.cores.iter().map(|c| c.usage as f64).sum::<f64>() / req.cores.len() as f64
            };

            let mut history = self.realtime_history.lock().unwrap();
            let entry = history.entry(req.hostname.clone()).or_default();
            entry.push((now, avg_cpu));
            if entry.len() > REALTIME_MAX_POINTS {
                entry.remove(0);
            }
        }

        self.state.lock().unwrap().insert(req.hostname.clone(), req);
        Ok(Response::new(MetricsResponse { success: true }))
    }
}

// ── Tab ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Tab { Hosts = 0, RealtimeChart = 1, MinuteChart = 2 }

impl Tab {
    fn index(self) -> usize { self as usize }
    fn from_index(i: usize) -> Self {
        match i { 1 => Tab::RealtimeChart, 2 => Tab::MinuteChart, _ => Tab::Hosts }
    }
    fn prev(self) -> Self { Self::from_index(if self.index() == 0 { 2 } else { self.index() - 1 }) }
    fn next(self) -> Self { Self::from_index((self.index() + 1) % 3) }
}

// ── main ─────────────────────────────────────────────────────────────────────

async fn load_initial_history(
    pool: &sqlx::PgPool,
    minute_history: &MinuteHistory,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT hostname, cpu_usage, created_at
        FROM (
            SELECT
                hostname,
                cpu_usage,
                created_at,
                ROW_NUMBER() OVER (
                    PARTITION BY hostname
                    ORDER BY created_at DESC
                ) AS rn
            FROM host_metrics
        ) ranked
        WHERE rn <= 20
        ORDER BY hostname, created_at ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut mh = minute_history.lock().unwrap();

    for row in rows {
        use sqlx::Row;

        let hostname: String          = row.get("hostname");
        let cpu_usage: Vec<f64>       = row.get("cpu_usage");
        let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");

        let avg_cpu = if cpu_usage.is_empty() {
            0.0
        } else {
            cpu_usage.iter().sum::<f64>() / cpu_usage.len() as f64
        };

        let unix_ts = created_at.timestamp() as f64
            + created_at.timestamp_subsec_millis() as f64 / 1000.0;

        mh.entry(hostname)
            .or_default()
            .push((unix_ts, avg_cpu));
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state: SharedState           = Arc::new(Mutex::new(HashMap::new()));
    let realtime_history: RealtimeHistory = Arc::new(Mutex::new(HashMap::new()));
    let minute_history: MinuteHistory     = Arc::new(Mutex::new(HashMap::new()));

    let addr = "0.0.0.0:50051".parse()?;

    dotenv().ok();
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = PgPoolOptions::new().max_connections(5).connect(&database_url).await?;

    if let Err(e) = load_initial_history(&pool, &minute_history).await {
        eprintln!("Error load history {}", e);
    }

    // gRPC
    {
        let s  = Arc::clone(&state);
        let rt = Arc::clone(&realtime_history);
        tokio::spawn(async move {
            Server::builder()
                .add_service(MetricsCollectorServer::new(MyCollector {
                    state: s,
                    realtime_history: rt,
                }))
                .serve(addr)
                .await
                .unwrap();
        });
    }

    // DB-воркер (раз в минуту)
    {
        let ws = Arc::clone(&state);
        let wp = pool.clone();
        let wm = Arc::clone(&minute_history);
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(60));
            loop {
                tick.tick().await;

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();

                let snapshot = { ws.lock().unwrap().clone() };

                for (hostname, metrics) in &snapshot {
                    let cpu_values: Vec<f64> =
                        metrics.cores.iter().map(|c| c.usage as f64).collect();
                    let avg_cpu = if cpu_values.is_empty() { 0.0 }
                        else { cpu_values.iter().sum::<f64>() / cpu_values.len() as f64 };

                    {
                        let mut mh = wm.lock().unwrap();
                        let entry = mh.entry(hostname.clone()).or_default();
                        entry.push((now, avg_cpu));
                        if entry.len() > 20 { entry.remove(0); }
                    }

                    let res = sqlx::query(
                        "INSERT INTO host_metrics (hostname, cpu_usage, memory_usage) VALUES ($1, $2, $3)",
                    )
                    .bind(hostname)
                    .bind(&cpu_values)
                    .bind(metrics.memory_usage as f64)
                    .execute(&wp)
                    .await;

                    if res.is_ok() {
                        let _ = sqlx::query(r#"
                            DELETE FROM host_metrics
                            WHERE id IN (
                                SELECT id FROM host_metrics
                                WHERE hostname = $1
                                ORDER BY created_at DESC OFFSET 20
                            )"#)
                        .bind(hostname)
                        .execute(&wp)
                        .await;
                    }
                }
            }
        });
    }

    // TUI
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut current_tab = Tab::Hosts;
    let mut tab_rects: [Rect; 3] = [Rect::default(); 3];

    // Пересчитанные данные живут здесь, снаружи draw
    let mut rs = RenderState::empty();

    loop {
        // ── 1. Готовим данные ВНЕ draw ──────────────────────────────────────
        rs = RenderState::refresh(&state, &realtime_history, &minute_history);

        // ── 2. Отрисовка — только читаем rs, никаких lock'ов/аллокаций ──────
        terminal.draw(|f| {
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(3),
                ])
                .split(size);

            // Заголовок
            f.render_widget(
                Paragraph::new(" RUST SYSTEM MONITOR [gRPC] ")
                    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                    .block(Block::default().borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::White))),
                chunks[0],
            );

            // Контент
            match current_tab {
                Tab::Hosts         => render_hosts(f, chunks[1], &rs),
                Tab::RealtimeChart => render_realtime_chart(f, chunks[1], &rs),
                Tab::MinuteChart   => render_minute_chart(f, chunks[1], &rs),
            }

            // Таббар
            f.render_widget(
                Tabs::new(vec![
                    Line::from(" [1] Hosts "),
                    Line::from(" [2] Real-Time Chart "),
                    Line::from(" [3] Minute Chart "),
                ])
                .select(current_tab.index())
                .style(Style::default().fg(Color::Gray))
                .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .divider("|")
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White))
                    .title(Span::styled(
                        " Tabs (click / 1·2·3 / ←→ / Tab) | q — quit ",
                        Style::default().fg(Color::DarkGray),
                    ))),
                chunks[2],
            );

            // Зоны кликов — массив фиксированного размера, без Vec
            let tab_w = chunks[2].width / 3;
            for i in 0..3usize {
                tab_rects[i] = Rect {
                    x: chunks[2].x + i as u16 * tab_w,
                    y: chunks[2].y,
                    width: tab_w,
                    height: chunks[2].height,
                };
            }
        })?;

        // ── 3. Ввод ──────────────────────────────────────────────────────────
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q')            => break,
                    KeyCode::Char('1')            => current_tab = Tab::Hosts,
                    KeyCode::Char('2')            => current_tab = Tab::RealtimeChart,
                    KeyCode::Char('3')            => current_tab = Tab::MinuteChart,
                    KeyCode::Tab | KeyCode::Right => current_tab = current_tab.next(),
                    KeyCode::Left                 => current_tab = current_tab.prev(),
                    _ => {}
                },
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(_), column, row, ..
                }) => {
                    for (i, rect) in tab_rects.iter().enumerate() {
                        if column >= rect.x && column < rect.x + rect.width
                            && row >= rect.y && row < rect.y + rect.height
                        {
                            current_tab = Tab::from_index(i);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

// ── render-функции: только читают RenderState, ничего не аллоцируют ──────────

fn render_hosts(f: &mut ratatui::Frame, area: Rect, rs: &RenderState) {
    let items: Vec<ListItem> = rs.hosts.iter().map(|row| {
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!(" HOST: {} ", row.hostname),
                Style::default().fg(row.color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("| MEM: {:.2} Gb", row.memory_gb),
                Style::default().fg(Color::Gray),
            ),
        ])];
        for &(id, usage) in &row.cores {
            lines.push(Line::from(vec![
                Span::styled(format!("  └─ Core {id}: "), Style::default().fg(row.color)),
                Span::styled(format!("{usage:.2}%"),      Style::default().fg(Color::White)),
            ]));
        }
        lines.push(Line::from(""));
        ListItem::new(lines)
    }).collect();

    f.render_widget(
        List::new(items).block(Block::default()
            .title(Span::styled(" Connected Agents ", Style::default().fg(Color::Magenta)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::White))),
        area,
    );
}

fn render_realtime_chart(f: &mut ratatui::Frame, area: Rect, rs: &RenderState) {
    if rs.realtime.is_empty() {
        f.render_widget(
            Paragraph::new("Нет данных. Ожидание агентов...")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().title(" Real-Time CPU (avg %) ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White))),
            area,
        );
        return;
    }

    // &s.points — слайс из уже готового Vec, Dataset не копирует данные
    let datasets: Vec<Dataset> = rs.realtime.iter().map(|s| {
        Dataset::default()
            .name(s.label.as_str())
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(s.color))
            .data(&s.points)
    }).collect();

    f.render_widget(
        Chart::new(datasets)
            .block(Block::default()
                .title(Span::styled(
                    " Real-Time CPU avg % (last 60 s) ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::White)))
            .x_axis(Axis::default()
                .title("Секунды")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, rs.realtime_max_x])
                .labels(vec![Span::raw("−60s"), Span::raw("−30s"), Span::raw("сейчас")]))
            .y_axis(Axis::default()
                .title("CPU %")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::raw("0"), Span::raw("25"), Span::raw("50"),
                    Span::raw("75"), Span::raw("100"),
                ])),
        area,
    );
}

fn render_minute_chart(f: &mut ratatui::Frame, area: Rect, rs: &RenderState) {
    if rs.minute.is_empty() {
        f.render_widget(
            Paragraph::new("Нет данных. Первый снимок появится через минуту...")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().title(" Minute CPU Chart ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White))),
            area,
        );
        return;
    }

    let datasets: Vec<Dataset> = rs.minute.iter().map(|s| {
        Dataset::default()
            .name(s.label.as_str())
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(s.color))
            .data(&s.points)
    }).collect();

    f.render_widget(
        Chart::new(datasets)
            .block(Block::default()
                .title(Span::styled(
                    " CPU avg % — снимки каждую минуту (последние 20) ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::White)))
            .x_axis(Axis::default()
                .title("Минуты")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, rs.minute_max_x])
                .labels(vec![Span::raw("старые"), Span::raw(""), Span::raw("свежие")]))
            .y_axis(Axis::default()
                .title("CPU %")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::raw("0"), Span::raw("25"), Span::raw("50"),
                    Span::raw("75"), Span::raw("100"),
                ])),
        area,
    );
}

use std::{collections::HashMap, sync::{Arc, Mutex}, time::Duration, io};
use tokio::time::sleep;
use tonic::{transport::Server, Request, Response, Status};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, List, ListItem},
    Terminal,
    text::{Line, Span}, // Добавлено для гибкой стилизации строк
};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

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

type SharedState = Arc<Mutex<HashMap<String, MetricsRequest>>>;

pub struct MyCollector {
    state: SharedState,
}

#[tonic::async_trait]
impl MetricsCollector for MyCollector {
    async fn send_metrics(
        &self,
        request: Request<MetricsRequest>,
    ) -> Result<Response<MetricsResponse>, Status> {
        let req = request.into_inner();
        let mut data = self.state.lock().unwrap();
        data.insert(req.hostname.clone(), req);
        Ok(Response::new(MetricsResponse { success: true }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state: SharedState = Arc::new(Mutex::new(HashMap::new()));
    let addr = "0.0.0.0:50051".parse()?;

    let server_state = Arc::clone(&state);
    tokio::spawn(async move {
        let collector = MyCollector { state: server_state };
        Server::builder()
            .add_service(MetricsCollectorServer::new(collector))
            .serve(addr)
            .await
            .unwrap();
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
                .split(f.size());

            let title = Paragraph::new(" RUST SYSTEM MONITOR [gRPC] ")
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White))); // Фиолетовая рамка
            f.render_widget(title, chunks[0]);

            let data = state.lock().unwrap();
            
            let mut sorted_metrics: Vec<_> = data.values().collect();
            sorted_metrics.sort_by(|a, b| a.hostname.cmp(&b.hostname));

            let items: Vec<ListItem> = sorted_metrics.iter().enumerate().map(|(i, metrics)| {
                let host_color = NEON_PALETTE[i % NEON_PALETTE.len()];
                
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(format!(" HOST: {} ", metrics.hostname), Style::default().fg(host_color).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("| MEM: {:.2} Gb", metrics.memory_usage), Style::default().fg(Color::Gray)),
                    ])
                ];
                
                for core in &metrics.cores {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  └─ Core {}: ", core.core_id), Style::default().fg(host_color)),
                        Span::styled(format!("{:.2}%", core.usage), Style::default().fg(Color::White)),
                    ]));
                }
                
                lines.push(Line::from(""));
                
                ListItem::new(lines)
            }).collect();

            let list = List::new(items)
                .block(Block::default()
                    .title(Span::styled(" Connected Agents ", Style::default().fg(Color::Magenta)))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White)));
            
            f.render_widget(list, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

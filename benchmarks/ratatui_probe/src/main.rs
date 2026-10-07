use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use std::sync::mpsc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|v| v.as_str()).unwrap_or("viewport");
    let count: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(10000);
    let frames: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(200);
    let history: Vec<Line<'static>> = (0..count).map(|i| Line::from(format!("Message {i:06}  |  Agent completed a change. Read its output, inspect the diff, or send a follow-up."))).collect();
    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
    let draw = |terminal: &mut Terminal<TestBackend>| {
        terminal.draw(|f| {
            let panels = Layout::horizontal([Constraint::Length(30), Constraint::Min(1)]).split(f.area());
            f.render_widget(Paragraph::new("Project alpha\n  Current thread\n  Research\n  Review\n\nAgents\n  Fable\n  Astra\n  Opus")
                .block(Block::default().title(" Projects ").borders(Borders::ALL))
                .style(Style::default().fg(Color::Cyan)), panels[0]);
            let visible = panels[1].height.saturating_sub(2) as usize;
            let start = history.len().saturating_sub(visible);
            let (lines, scroll) = if mode == "full-history" {
                (history.clone(), (start.min(u16::MAX as usize) as u16, 0))
            } else {
                (history[start..].to_vec(), (0, 0))
            };
            f.render_widget(Paragraph::new(lines).scroll(scroll)
                .block(Block::default().title(" Conversation ").borders(Borders::ALL)), panels[1]);
        }).unwrap();
    };
    draw(&mut terminal);
    let start = Instant::now();
    let mut draws = 0;
    if mode == "idle-poll" || mode == "idle-events" {
        let duration = std::time::Duration::from_secs(5);
        if mode == "idle-poll" {
            while start.elapsed() < duration {
                std::thread::sleep(std::time::Duration::from_millis(120));
                draw(&mut terminal);
                draws += 1;
            }
        } else {
            let (_keep_open, rx) = mpsc::channel::<()>();
            let _ = rx.recv_timeout(duration);
        }
    } else {
        for _ in 0..frames {
            draw(&mut terminal);
            draws += 1;
        }
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut checksum = 14695981039346656037u64;
    for cell in &terminal.backend().buffer().content {
        for byte in cell.symbol().bytes() {
            checksum = (checksum ^ byte as u64).wrapping_mul(1099511628211);
        }
    }
    println!(
        "{{\"mode\":\"{mode}\",\"history\":{count},\"draws\":{draws},\"elapsed_ms\":{elapsed_ms},\"screen_checksum\":{checksum}}}"
    );
}

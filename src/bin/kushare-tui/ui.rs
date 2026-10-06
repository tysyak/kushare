use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};

use crate::app::{App, HomeChoice, Screen, SendStatus};

const ACCENT: Color = Color::Cyan;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    match &app.screen {
        Screen::Home { selected } => draw_home(frame, chunks[0], *selected),
        Screen::Receive(receive) => draw_receive(frame, chunks[0], receive),
        Screen::Send(send) => draw_send(frame, chunks[0], send),
    }

    draw_status_line(frame, chunks[1], app);
}

fn title_block(title: &str) -> Block<'_> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
}

fn draw_status_line(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.status_line.is_empty() {
        "kushare-tui — q: salir".to_string()
    } else {
        app.status_line.clone()
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn draw_home(frame: &mut Frame, area: Rect, selected: HomeChoice) {
    let box_area = centered(area, 40, 8);
    let block = title_block("kushare");
    frame.render_widget(block, box_area);

    let inner = Rect {
        x: box_area.x + 2,
        y: box_area.y + 2,
        width: box_area.width.saturating_sub(4),
        height: box_area.height.saturating_sub(4),
    };

    let items = [
        ("Enviar", HomeChoice::Enviar),
        ("Recibir", HomeChoice::Recibir),
    ]
    .into_iter()
    .map(|(label, choice)| {
        let style = if choice == selected {
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        ListItem::new(Line::from(Span::styled(format!("  {label}  "), style)))
    });

    frame.render_widget(
        List::new(items).highlight_style(Style::default()),
        Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 2,
        },
    );

    let hint = Paragraph::new("↑/↓ elegir · Enter confirmar · q salir")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(
        hint,
        Rect {
            x: box_area.x,
            y: box_area.y + box_area.height.saturating_sub(2),
            width: box_area.width,
            height: 1,
        },
    );
}

fn draw_receive(frame: &mut Frame, area: Rect, receive: &crate::app::ReceiveScreen) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(3)])
        .split(area);

    let visibility = if receive.visible {
        Span::styled("visible para todos", Style::default().fg(Color::Green))
    } else {
        Span::styled("oculto", Style::default().fg(Color::Yellow))
    };

    let name_line = if receive.editing_name {
        Line::from(vec![
            Span::raw("Nombre: "),
            Span::styled(
                format!("{}_", receive.name_input),
                Style::default().fg(ACCENT),
            ),
        ])
    } else {
        Line::from(vec![
            Span::raw("Nombre: "),
            Span::styled(receive.name_input.clone(), Style::default().fg(ACCENT)),
            Span::raw("  (e para editar)"),
        ])
    };

    let info = Paragraph::new(vec![
        name_line,
        Line::from(vec![Span::raw("Estado: "), visibility]),
        Line::from("v: alternar visibilidad · e: editar nombre · Esc: volver"),
    ])
    .block(title_block("Recibir"));
    frame.render_widget(info, chunks[0]);

    let log_items: Vec<ListItem> = receive
        .log
        .iter()
        .rev()
        .take(chunks[1].height.saturating_sub(2) as usize)
        .rev()
        .map(|line| ListItem::new(line.as_str()))
        .collect();
    let log = List::new(log_items).block(title_block("Actividad"));
    frame.render_widget(log, chunks[1]);
}

fn draw_send(frame: &mut Frame, area: Rect, send: &crate::app::SendScreen) {
    match &send.status {
        SendStatus::PickingFile => draw_send_picking_file(frame, area, send),
        SendStatus::PickingDevice => draw_send_picking_device(frame, area, send),
        SendStatus::Sending { bytes, total } => {
            draw_send_sending(frame, area, send, *bytes, *total)
        }
        SendStatus::Done => draw_send_result(frame, area, "Archivo enviado.", Color::Green),
        SendStatus::Failed(reason) => {
            draw_send_result(frame, area, &format!("Error: {reason}"), Color::Red)
        }
    }
}

fn draw_send_picking_file(frame: &mut Frame, area: Rect, send: &crate::app::SendScreen) {
    let mut lines = vec![Line::from("¿Qué archivo quieres enviar?")];
    if send.yazi_available {
        lines.push(Line::from("y: elegir con yazi"));
    }
    lines.push(Line::from("p: escribir la ruta"));
    if send.editing_path {
        lines.push(Line::from(vec![
            Span::raw("Ruta: "),
            Span::styled(format!("{}_", send.path_input), Style::default().fg(ACCENT)),
        ]));
    }
    lines.push(Line::from("Esc: volver"));

    let block = Paragraph::new(lines).block(title_block("Enviar"));
    frame.render_widget(block, area);
}

fn draw_send_picking_device(frame: &mut Frame, area: Rect, send: &crate::app::SendScreen) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);

    let file_name = send
        .file
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(format!("Archivo: {file_name}")).block(title_block("Enviar")),
        chunks[0],
    );

    let items: Vec<ListItem> = if send.devices.is_empty() {
        vec![ListItem::new("Buscando dispositivos cercanos (mDNS)...")]
    } else {
        send.devices
            .iter()
            .map(|d| ListItem::new(format!("{}  [{}]", d.name, d.addr)))
            .collect()
    };
    let mut state = ListState::default();
    if !send.devices.is_empty() {
        state.select(Some(send.selected));
    }
    let list = List::new(items)
        .block(title_block(
            "Dispositivos cercanos (↑/↓ elegir · Enter enviar · Esc volver)",
        ))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, chunks[1], &mut state);
}

fn draw_send_sending(
    frame: &mut Frame,
    area: Rect,
    send: &crate::app::SendScreen,
    bytes: u64,
    total: u64,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(3)])
        .split(area);

    let file_name = send
        .file
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(format!("Enviando {file_name}...")).block(title_block("Enviar")),
        chunks[0],
    );

    let ratio = if total > 0 {
        (bytes as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL))
        .gauge_style(Style::default().fg(ACCENT))
        .ratio(ratio)
        .label(format!("{bytes}/{total} bytes"));
    frame.render_widget(gauge, chunks[1]);
}

fn draw_send_result(frame: &mut Frame, area: Rect, message: &str, color: Color) {
    let block = Paragraph::new(vec![
        Line::from(Span::styled(message, Style::default().fg(color))),
        Line::from(""),
        Line::from("Enter/Esc: volver al menú"),
    ])
    .block(title_block("Enviar"));
    frame.render_widget(block, area);
}

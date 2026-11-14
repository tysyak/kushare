//! Dibujo de la TUI, con la misma organización que la GUI: pestañas arriba,
//! panel del teléfono a la izquierda (si cabe) y la página a la derecha.

use crate::app::{App, InputKind, Tab};
use kushare_i18n::{t, tf};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, LineGauge, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};

/// Violeta de kushare, legible sobre fondo oscuro y claro.
const ACCENT: Color = Color::Rgb(0xC7, 0x7D, 0xFF);
const DIM: Color = Color::Gray;
const ERROR: Color = Color::Rgb(0xFF, 0x8A, 0x80);
/// Ancho mínimo de la terminal para mostrar el panel del teléfono al lado.
const PANEL_MIN_WIDTH: u16 = 100;
const PANEL_WIDTH: u16 = 40;

pub fn draw(frame: &mut Frame, app: &App) {
    let [tabs, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_tabs(frame, tabs, app);
    if body.width >= PANEL_MIN_WIDTH {
        let [panel, page] =
            Layout::horizontal([Constraint::Length(PANEL_WIDTH), Constraint::Min(40)]).areas(body);
        draw_panel(frame, panel, app);
        draw_page(frame, page, app);
    } else {
        draw_page(frame, body, app);
    }
    draw_footer(frame, footer, app);
}

fn block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).bold(),
        ))
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| Line::from(format!("{} {}", i + 1, tab.label())))
        .collect();
    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(selected)
        .block(block("ku"))
        .style(Style::default().fg(DIM))
        .highlight_style(Style::default().fg(Color::Black).bg(ACCENT).bold())
        .divider(" ");
    frame.render_widget(tabs, area);
}

// --- Panel del teléfono -----------------------------------------------------

fn draw_panel(frame: &mut Frame, area: Rect, app: &App) {
    let status = status_lines(app);
    let media = media_lines(app);
    let status_height = status.len() as u16 + 2;
    let media_height = if media.is_some() { 5 } else { 0 };
    let [st, md, notifs] = Layout::vertical([
        Constraint::Length(status_height),
        Constraint::Length(media_height),
        Constraint::Min(3),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(status).block(block(t("gui.nav.phone"))), st);
    if let Some((lines, ratio)) = media {
        draw_media(frame, md, lines, ratio);
    }
    draw_notifications(frame, notifs, app, true);
}

/// Estado del teléfono: nombre, conexión, batería, timbre, llamada.
fn status_lines(app: &App) -> Vec<Line<'static>> {
    if !app.connected {
        return vec![
            Line::from(t("tui.daemon_off").to_string()).fg(ERROR),
            Line::from(t("tui.daemon_off_detail").to_string()).fg(DIM),
        ];
    }
    let Some(p) = app.phone() else {
        return vec![
            Line::from(t("gui.panel.disconnected").to_string()).bold(),
            Line::from(t("gui.panel.disconnected_detail").to_string()).fg(DIM),
        ];
    };
    let via = via(&p.4);
    let mut lines = vec![
        Line::from(format!("▯ {}", p.1)).bold(),
        Line::from(vec![
            Span::styled("● ", Style::default().fg(ACCENT)),
            Span::raw(format!("{} · {via}", t("gui.panel.connected"))),
        ]),
    ];
    if let Some(st) = app.phone_state() {
        let (has, pct, charging) = st.0;
        if has {
            let style = if pct <= 15 && !charging {
                Style::default().fg(ERROR)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                format!("▮ {pct} %{}", if charging { " ⚡" } else { "" }),
                style,
            ));
        }
        if !st.1.is_empty() {
            let ringer = match st.1.as_str() {
                "vibrate" => t("gui.phone.ringer_vibrate"),
                "silent" => t("gui.phone.ringer_silent"),
                _ => t("gui.phone.ringer_normal"),
            };
            lines.push(Line::from(format!("♪ {ringer}")).fg(DIM));
        }
        let call = &st.2;
        if call.0 != "idle" {
            let who = if call.2.is_empty() { &call.1 } else { &call.2 };
            let label = match call.0.as_str() {
                "ringing" => tf("gui.phone.call_incoming", &[("who", who)]),
                "dialing" => tf("gui.phone.call_dialing", &[("who", who)]),
                _ => tf("gui.phone.call_active", &[("who", who)]),
            };
            lines.push(Line::from(format!("☎ {label}")).fg(ACCENT).bold());
        }
        if st.6 {
            lines.push(
                Line::from(format!("◉ {} (f)", t("gui.panel.find_stop")))
                    .fg(Color::Black)
                    .bg(ACCENT),
            );
        }
    }
    lines
}

fn via(transport: &str) -> &'static str {
    if transport.starts_with("bluetooth") && transport.contains("+ datos wifi") {
        "Bluetooth + Wi-Fi"
    } else if transport.starts_with("bluetooth") {
        "Bluetooth"
    } else {
        "Wi-Fi"
    }
}

/// Lo que suena: (líneas, progreso 0–1 y tiempos).
/// Progreso de lo que suena: (fracción 0–1, «1:15 / -4:01»).
type Progress = Option<(f64, String)>;

fn media_lines(app: &App) -> Option<(Vec<Line<'static>>, Progress)> {
    let st = app.phone_state()?;
    let p =
        st.3.iter()
            .find(|p| p.5 == "playing")
            .or_else(|| st.3.iter().find(|p| !p.2.is_empty()))?;
    let playing = p.5 == "playing";
    let title = if p.2.is_empty() { &p.1 } else { &p.2 };
    let artist = if p.3.is_empty() { &p.1 } else { &p.3 };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                if playing { "▶ " } else { "⏸ " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(title.clone(), Style::default().bold()),
        ]),
        Line::from(format!("  {artist}")).fg(DIM),
    ];
    let (_, position, duration, at) = &p.10;
    let progress = (*duration > 0).then(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let pos = if playing {
            position + (now - at).max(0)
        } else {
            *position
        }
        .clamp(0, *duration);
        let fmt = |ms: i64| format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60);
        (
            pos as f64 / *duration as f64,
            format!("{} / -{}", fmt(pos), fmt(duration - pos)),
        )
    });
    Some((lines, progress))
}

fn draw_media(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>, progress: Progress) {
    let outer = block(t("tui.now_playing"));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [text, gauge] =
        Layout::vertical([Constraint::Length(2), Constraint::Length(1)]).areas(inner);
    frame.render_widget(Paragraph::new(lines), text);
    if let Some((ratio, label)) = progress {
        frame.render_widget(
            LineGauge::default()
                .ratio(ratio.clamp(0.0, 1.0))
                .label(label)
                .filled_style(Style::default().fg(ACCENT))
                .unfilled_style(Style::default().fg(Color::DarkGray)),
            gauge,
        );
    }
}

fn draw_notifications(frame: &mut Frame, area: Rect, app: &App, compact: bool) {
    let title = format!("{} ({})", t("gui.nav.notifications"), app.notifs.len());
    let width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .notifs
        .iter()
        .map(|n| {
            let mut lines = vec![
                Line::from(format!("{} · {}", n.2, fmt_time(n.5))).fg(DIM),
                Line::from(cut(&n.3, width)).bold(),
            ];
            if !n.4.is_empty() {
                let body_lines = if compact { 2 } else { 4 };
                for l in wrap(&n.4, width).into_iter().take(body_lines) {
                    lines.push(Line::from(l));
                }
            }
            if !compact && !n.7.is_empty() {
                let actions: Vec<String> = n.7.iter().map(|a| format!("[{}]", a.1)).collect();
                lines.push(Line::from(actions.join(" ")).fg(ACCENT));
            }
            lines.push(Line::from(""));
            ListItem::new(Text::from(lines))
        })
        .collect();
    let mut state = ListState::default();
    if app.tab == Tab::Notifications && !app.notifs.is_empty() {
        state.select(Some(app.sel("notifs")));
    }
    let list = List::new(items)
        .block(block(&title))
        .highlight_style(Style::default().bg(Color::Rgb(0x3A, 0x22, 0x4A)));
    frame.render_stateful_widget(list, area, &mut state);
}

// --- Páginas ----------------------------------------------------------------

fn draw_page(frame: &mut Frame, area: Rect, app: &App) {
    if !app.connected {
        let p = Paragraph::new(vec![
            Line::from(t("tui.daemon_off").to_string()).fg(ERROR).bold(),
            Line::from(t("tui.daemon_off_detail").to_string()),
        ])
        .block(block("kushare"))
        .wrap(Wrap { trim: false });
        frame.render_widget(p, area);
        return;
    }
    let needs_phone = !matches!(app.tab, Tab::Home | Tab::Devices | Tab::Clipboard);
    if needs_phone && app.phone().is_none() {
        let p = Paragraph::new(vec![
            Line::from(t("gui.phone.none").to_string()).bold(),
            Line::from(t("gui.panel.disconnected_detail").to_string()).fg(DIM),
        ])
        .block(block(app.tab.label()));
        frame.render_widget(p, area);
        return;
    }
    match app.tab {
        Tab::Home => draw_home(frame, area, app),
        Tab::Phone => draw_phone(frame, area, app),
        Tab::Messages => draw_messages(frame, area, app),
        Tab::Calls => draw_calls(frame, area, app),
        Tab::Notifications => draw_notifications(frame, area, app, false),
        Tab::Clipboard => draw_clipboard(frame, area, app),
        Tab::Devices => draw_devices(frame, area, app),
        Tab::Send => draw_send(frame, area, app),
    }
}

fn draw_home(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = vec![
        Line::from("kushare").fg(ACCENT).bold(),
        Line::from(app.device_name.clone()).fg(DIM),
        Line::from(""),
    ];
    match app.phone() {
        Some(p) => {
            lines.push(Line::from(t("gui.home.connected").to_string()).bold());
            lines.push(Line::from(format!("{} · {}", p.1, via(&p.4))));
        }
        None => {
            lines.push(Line::from(t("gui.home.waiting").to_string()).bold());
            lines.push(Line::from(t("gui.home.waiting_detail").to_string()).fg(DIM));
        }
    }
    lines.push(Line::from(""));
    lines.push(
        Line::from(t("tui.keys_title").to_string())
            .fg(ACCENT)
            .bold(),
    );
    for help in [
        t("tui.keys_tabs"),
        t("tui.keys_move"),
        t("tui.keys_media"),
        t("tui.keys_find"),
        t("tui.keys_quit"),
    ] {
        lines.push(Line::from(help.to_string()));
    }
    let p = Paragraph::new(lines)
        .block(block(t("gui.nav.home")))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn draw_phone(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = status_lines(app);
    if let Some((media, progress)) = media_lines(app) {
        lines.push(Line::from(""));
        lines.extend(media);
        if let Some((_, label)) = progress {
            lines.push(Line::from(format!("  {label}")).fg(DIM));
        }
    }
    if let Some(st) = app.phone_state()
        && !st.5.is_empty()
    {
        {
            lines.push(Line::from(""));
            lines.push(
                Line::from(t("gui.phone.output").to_string())
                    .fg(ACCENT)
                    .bold(),
            );
            for (_, name, active) in &st.5 {
                lines.push(Line::from(format!(
                    "{} {name}",
                    if *active { "●" } else { "○" }
                )));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(t("tui.keys_phone").to_string()).fg(DIM));
    let p = Paragraph::new(lines)
        .block(block(t("gui.nav.phone")))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn draw_messages(frame: &mut Frame, area: Rect, app: &App) {
    // Ancha: lista y conversación; estrecha: una u otra.
    let wide = area.width >= 90;
    let (list_area, conv_area) = match (wide, app.conversation.is_some()) {
        (true, _) => {
            let [l, c] =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .areas(area);
            (Some(l), Some(c))
        }
        (false, false) => (Some(area), None),
        (false, true) => (None, Some(area)),
    };
    if let Some(area) = list_area {
        let width = area.width.saturating_sub(4) as usize;
        let items: Vec<ListItem> = app
            .threads
            .iter()
            .map(|th| {
                let who = if th.2.is_empty() { &th.1 } else { &th.2 };
                let mut name = Line::from(vec![
                    Span::raw(cut(who, width.saturating_sub(14))),
                    Span::styled(format!("  {}", fmt_time(th.4)), Style::default().fg(DIM)),
                ]);
                if th.5 {
                    name = name.bold();
                }
                let snippet: String = th.3.replace('\n', " ");
                ListItem::new(vec![name, Line::from(cut(&snippet, width)).fg(DIM)])
            })
            .collect();
        let mut state = ListState::default();
        if app.conversation.is_none() && !app.threads.is_empty() {
            state.select(Some(app.sel("threads")));
        }
        let title = if app.loading && app.threads.is_empty() {
            t("gui.common.loading")
        } else {
            t("gui.nav.messages")
        };
        frame.render_stateful_widget(
            List::new(items)
                .block(block(title))
                .highlight_style(Style::default().bg(Color::Rgb(0x3A, 0x22, 0x4A))),
            area,
            &mut state,
        );
    }
    if let Some(area) = conv_area {
        let Some((id, messages)) = &app.conversation else {
            frame.render_widget(
                Paragraph::new(t("gui.sms.pick").to_string())
                    .fg(DIM)
                    .block(block("")),
                area,
            );
            return;
        };
        let th = app.threads.iter().find(|th| th.0 == *id);
        let title = th
            .map(|th| {
                if th.2.is_empty() {
                    th.1.clone()
                } else {
                    th.2.clone()
                }
            })
            .unwrap_or_default();
        let width = area.width.saturating_sub(4) as usize;
        let mut lines = Vec::new();
        for m in messages.iter().rev() {
            let (outgoing, text) = (m.4, &m.2);
            let meta = fmt_time(m.3);
            for l in wrap(text, width.saturating_sub(6)) {
                lines.push(if outgoing {
                    Line::from(l).fg(ACCENT).right_aligned()
                } else {
                    Line::from(l)
                });
            }
            lines.push(if outgoing {
                Line::from(meta).fg(DIM).right_aligned()
            } else {
                Line::from(meta).fg(DIM)
            });
            lines.push(Line::from(""));
        }
        // Lo más reciente abajo, a la vista.
        let inner_height = area.height.saturating_sub(2) as usize;
        let scroll = lines.len().saturating_sub(inner_height) as u16;
        frame.render_widget(
            Paragraph::new(lines)
                .block(block(&format!("{title} · w {}", t("tui.write"))))
                .scroll((scroll, 0)),
            area,
        );
    }
}

fn draw_calls(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .calls
        .iter()
        .map(|c| {
            let (icon, color, kind) = match c.4.as_str() {
                "outgoing" => ("↗", ACCENT, t("gui.calls.kind_outgoing")),
                "missed" => ("↙", ERROR, t("gui.calls.kind_missed")),
                "rejected" => ("✕", ERROR, t("gui.calls.kind_rejected")),
                "blocked" => ("⊘", ERROR, t("gui.calls.kind_blocked")),
                "voicemail" => ("✉", Color::Cyan, t("gui.calls.kind_voicemail")),
                _ => ("↙", Color::Cyan, t("gui.calls.kind_incoming")),
            };
            let who = if c.1.is_empty() { &c.0 } else { &c.1 };
            let mut detail = format!("{kind} · {}", fmt_time(c.2));
            if c.3 > 0 {
                detail = format!("{detail} · {}:{:02}", c.3 / 60, c.3 % 60);
            }
            if !c.1.is_empty() {
                detail = format!("{} · {detail}", c.0);
            }
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("{icon} "), Style::default().fg(color)),
                    Span::styled(cut(who, width), Style::default().bold()),
                ]),
                Line::from(format!("  {}", cut(&detail, width))).fg(DIM),
            ])
        })
        .collect();
    let mut state = ListState::default();
    if !app.calls.is_empty() {
        state.select(Some(app.sel("calls")));
    }
    let title = format!(
        "{} · Enter {} · d {}",
        t("gui.calls.recent"),
        t("gui.calls.call"),
        t("tui.dial")
    );
    frame.render_stateful_widget(
        List::new(items)
            .block(block(&title))
            .highlight_style(Style::default().bg(Color::Rgb(0x3A, 0x22, 0x4A))),
        area,
        &mut state,
    );
}

fn draw_clipboard(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .clipboard
        .iter()
        .map(|c| {
            ListItem::new(vec![
                Line::from(cut(&c.0.replace('\n', " "), width)),
                Line::from(format!("{} · {}", c.2, fmt_time(c.1))).fg(DIM),
            ])
        })
        .collect();
    let mut state = ListState::default();
    if !app.clipboard.is_empty() {
        state.select(Some(app.sel("clipboard")));
    }
    let title = format!("{} · Enter {}", t("gui.nav.clipboard"), t("tui.copy"));
    frame.render_stateful_widget(
        List::new(items)
            .block(block(&title))
            .highlight_style(Style::default().bg(Color::Rgb(0x3A, 0x22, 0x4A))),
        area,
        &mut state,
    );
}

fn draw_devices(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .peers
        .iter()
        .map(|p| {
            let state = if p.3 {
                Span::styled(
                    format!("● {} · {}", t("gui.panel.connected"), via(&p.4)),
                    Style::default().fg(ACCENT),
                )
            } else {
                Span::styled(format!("○ {}", t("tui.offline")), Style::default().fg(DIM))
            };
            ListItem::new(vec![
                Line::from(format!("{} [{}]", p.1, p.2)).bold(),
                Line::from(vec![Span::raw("  "), state]),
            ])
        })
        .collect();
    let mut state = ListState::default();
    if !app.peers.is_empty() {
        state.select(Some(app.sel("devices")));
    }
    let title = format!("{} · s {}", t("gui.nav.devices"), t("tui.scan"));
    frame.render_stateful_widget(
        List::new(items)
            .block(block(&title))
            .highlight_style(Style::default().bg(Color::Rgb(0x3A, 0x22, 0x4A))),
        area,
        &mut state,
    );
}

fn draw_send(frame: &mut Frame, area: Rect, app: &App) {
    let file = app
        .send_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| t("tui.no_file").to_string());
    let lines = vec![
        Line::from(t("gui.home.send_detail").to_string()).fg(DIM),
        Line::from(""),
        Line::from(file).bold(),
        Line::from(""),
        Line::from(t("tui.keys_send").to_string()).fg(ACCENT),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(block(t("gui.nav.send")))
            .wrap(Wrap { trim: false }),
        area,
    );
}

// --- Pie: entrada de texto, aviso o ayuda --------------------------------------

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(input) = &app.input {
        let label = match &input.kind {
            InputKind::Sms => t("tui.input_sms"),
            InputKind::Dial => t("tui.input_dial"),
            InputKind::Reply(..) => t("tui.input_reply"),
            InputKind::SendPath => t("tui.input_path"),
        };
        Line::from(vec![
            Span::styled(
                format!(" {label}: "),
                Style::default().fg(Color::Black).bg(ACCENT),
            ),
            Span::raw(format!(" {}", input.text)),
            Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ])
    } else if let Some((toast, _)) = &app.toast {
        Line::from(format!(" {toast}")).fg(ACCENT).bold()
    } else {
        let tab_keys = match app.tab {
            Tab::Notifications => t("tui.keys_notifs"),
            Tab::Messages => t("tui.keys_messages"),
            _ => "",
        };
        Line::from(format!(" {}  {tab_keys}", t("tui.keys_short"))).fg(DIM)
    };
    frame.render_widget(Paragraph::new(line), area);
}

// --- Utilidades ---------------------------------------------------------------

/// Recorta a `width` caracteres con «…».
fn cut(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Parte un texto en líneas de como mucho `width` caracteres (por palabras).
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for para in s.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

/// Fecha local corta: «14:05» hoy, «12 nov 14:05» otro día.
fn fmt_time(ms: i64) -> String {
    fn local(secs: i64) -> libc::tm {
        let t = secs as libc::time_t;
        // SAFETY: `localtime_r` solo escribe en `tm`, que vive en esta pila.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            tm
        }
    }
    if ms <= 0 {
        return String::new();
    }
    let tm = local(ms / 1000);
    let now = local(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    );
    if tm.tm_year == now.tm_year && tm.tm_yday == now.tm_yday {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    } else {
        const MONTHS: [&str; 12] = [
            "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
        ];
        format!(
            "{} {} {:02}:{:02}",
            tm.tm_mday,
            MONTHS[tm.tm_mon.clamp(0, 11) as usize],
            tm.tm_hour,
            tm.tm_min
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_and_cut_respect_width() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
        assert_eq!(
            wrap("uno dos tres cuatro", 9),
            vec!["uno dos", "tres", "cuatro"]
        );
    }
}

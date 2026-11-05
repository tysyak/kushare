//! `kushare doctor`: diagnóstico de los problemas típicos (puerto, firewall,
//! mDNS, Bluetooth, servidor de notificaciones, otros puentes) con el
//! comando exacto para arreglar cada uno. Funciona con o sin el daemon.

use std::process::Command;

const LINK_PORT: u16 = crate::link::DEFAULT_LINK_PORT;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Bad,
    Info,
}

struct Report {
    lines: Vec<String>,
    problems: usize,
}

impl Report {
    fn line(&mut self, level: Level, text: impl AsRef<str>) {
        let mark = match level {
            Level::Ok => "✓",
            Level::Warn => "!",
            Level::Bad => "✗",
            Level::Info => "·",
        };
        if matches!(level, Level::Warn | Level::Bad) {
            self.problems += 1;
        }
        self.lines.push(format!("  {mark} {}", text.as_ref()));
    }

    fn fix(&mut self, command: impl AsRef<str>) {
        self.lines.push(format!("      → {}", command.as_ref()));
    }

    fn section(&mut self, title: &str) {
        self.lines.push(String::new());
        self.lines.push(title.to_string());
    }
}

/// Ejecuta un comando y devuelve su salida estándar si terminó bien.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

pub async fn run_doctor() -> anyhow::Result<()> {
    let mut r = Report {
        lines: vec!["Diagnóstico de kushare".into()],
        problems: 0,
    };

    // --- Daemon y puerto ---------------------------------------------------
    r.section("Daemon y puerto");
    let mut port = LINK_PORT;
    match kushare_dbus::connect().await {
        Ok(daemon) => {
            port = daemon.link_port().await.unwrap_or(LINK_PORT);
            let peers = daemon.peers().await.unwrap_or_default();
            r.line(
                Level::Ok,
                format!(
                    "el daemon está corriendo ({} emparejado(s), {} conectado(s))",
                    peers.len(),
                    peers.iter().filter(|p| p.3).count()
                ),
            );
            if port == LINK_PORT {
                r.line(
                    Level::Ok,
                    format!("el enlace escucha en el puerto {port}/tcp"),
                );
            } else {
                r.line(Level::Warn, format!("el puerto {LINK_PORT} estaba ocupado: el enlace usa {port}/tcp (cambia en cada arranque)"));
                r.fix(format!("libera el {LINK_PORT} o fija otro con `kushare --link-port <puerto>` y ábrelo en el firewall"));
            }
        }
        Err(_) => {
            r.line(Level::Warn, "el daemon no está corriendo");
            r.fix("systemctl --user enable --now kushare   (o lanza `kushare`)");
            match std::net::TcpListener::bind(("0.0.0.0", LINK_PORT)) {
                Ok(_) => r.line(Level::Ok, format!("el puerto {LINK_PORT}/tcp está libre")),
                Err(_) => {
                    let owner = run("ss", &["-ltnpH", &format!("sport = :{LINK_PORT}")])
                        .map(|s| s.split_whitespace().last().unwrap_or_default().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "otro programa".into());
                    r.line(Level::Warn, format!("el puerto {LINK_PORT}/tcp está ocupado por {owner}; el daemon usará otro libre"));
                }
            }
        }
    }

    // --- Red ----------------------------------------------------------------
    r.section("Red");
    let interfaces: Vec<(String, String)> = run("ip", &["-4", "-br", "addr"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let name = parts.next()?.to_string();
            let state = parts.next()?;
            let addr = parts.next()?.to_string();
            (name != "lo" && state != "DOWN").then_some((name, addr))
        })
        .collect();
    let default_iface = run("ip", &["route", "show", "default"]).and_then(|s| {
        s.split_whitespace()
            .skip_while(|w| *w != "dev")
            .nth(1)
            .map(str::to_string)
    });
    for (name, addr) in &interfaces {
        let mark = if Some(name) == default_iface.as_ref() {
            " (ruta por defecto)"
        } else {
            ""
        };
        r.line(Level::Info, format!("{name}: {addr}{mark}"));
    }
    r.line(
        Level::Info,
        "si el teléfono y el PC no se ven ni con `ping`, la red aísla a los clientes: empareja por Bluetooth (ver FAQ)",
    );

    // --- Firewall -------------------------------------------------------------
    r.section("Firewall");
    let port_rule = format!("{port}/tcp");
    if run("firewall-cmd", &["--state"]).is_some_and(|s| s.trim() == "running") {
        let zones = run("firewall-cmd", &["--get-active-zones"]).unwrap_or_default();
        // Formato: "zona\n  interfaces: a b\n  sources: ..."
        let mut current: Option<String> = None;
        let mut zone_ifaces: Vec<(String, Vec<String>)> = Vec::new();
        for line in zones.lines() {
            if !line.starts_with(char::is_whitespace) {
                current = Some(
                    line.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                );
            } else if let (Some(zone), Some(list)) =
                (&current, line.trim().strip_prefix("interfaces:"))
            {
                zone_ifaces.push((
                    zone.clone(),
                    list.split_whitespace().map(str::to_string).collect(),
                ));
            }
        }
        let relevant: Vec<&(String, Vec<String>)> = zone_ifaces
            .iter()
            .filter(|(_, ifs)| ifs.iter().any(|i| interfaces.iter().any(|(n, _)| n == i)))
            .collect();
        if relevant.is_empty() {
            r.line(
                Level::Info,
                "firewalld activo, pero ninguna interfaz con IP está en una zona activa",
            );
        }
        for (zone, ifs) in relevant {
            let services = run(
                "firewall-cmd",
                &[&format!("--zone={zone}"), "--list-services"],
            )
            .unwrap_or_default();
            let ports = run("firewall-cmd", &[&format!("--zone={zone}"), "--list-ports"])
                .unwrap_or_default();
            let target = run(
                "firewall-cmd",
                &["--permanent", &format!("--zone={zone}"), "--get-target"],
            )
            .unwrap_or_default();
            let open_all = matches!(target.trim(), "ACCEPT");
            let has_mdns = open_all || services.split_whitespace().any(|s| s == "mdns");
            let has_port = open_all || ports.split_whitespace().any(|p| p == port_rule);
            let where_ = format!("firewalld, zona `{zone}` ({})", ifs.join(", "));
            if has_port {
                r.line(Level::Ok, format!("{where_}: {port_rule} abierto"));
            } else {
                r.line(
                    Level::Bad,
                    format!(
                        "{where_}: {port_rule} cerrado (el teléfono no podrá conectarse por Wi-Fi)"
                    ),
                );
                r.fix(format!("sudo firewall-cmd --zone={zone} --permanent --add-port={port_rule} && sudo firewall-cmd --reload"));
            }
            if has_mdns {
                r.line(Level::Ok, format!("{where_}: mDNS permitido"));
            } else {
                r.line(
                    Level::Bad,
                    format!("{where_}: mDNS bloqueado (`kushare scan` no encontrará nada)"),
                );
                r.fix(format!("sudo firewall-cmd --zone={zone} --permanent --add-service=mdns && sudo firewall-cmd --reload"));
            }
        }
        r.line(Level::Info, "firewall-cmd: cada --add-service/--add-port en su propia llamada (algunas versiones no los admiten juntos)");
    } else if which("ufw") {
        match run("ufw", &["status"]) {
            Some(status) if status.contains("inactive") => {
                r.line(Level::Ok, "ufw instalado pero inactivo")
            }
            Some(status) => {
                let has_port = status
                    .lines()
                    .any(|l| l.starts_with(&port.to_string()) && l.contains("ALLOW"));
                let has_mdns = status
                    .lines()
                    .any(|l| l.starts_with("5353") && l.contains("ALLOW"));
                if has_port {
                    r.line(Level::Ok, format!("ufw: {port_rule} permitido"));
                } else {
                    r.line(Level::Bad, format!("ufw: {port_rule} no permitido"));
                    r.fix(format!("sudo ufw allow {port_rule}"));
                }
                if has_mdns {
                    r.line(Level::Ok, "ufw: mDNS (5353/udp) permitido");
                } else {
                    r.line(Level::Bad, "ufw: mDNS (5353/udp) no permitido");
                    r.fix("sudo ufw allow 5353/udp");
                }
            }
            None => {
                r.line(
                    Level::Warn,
                    "ufw instalado, pero leer su estado requiere root",
                );
                r.fix(format!("sudo ufw status   (y si está activo: sudo ufw allow {port_rule} && sudo ufw allow 5353/udp)"));
            }
        }
    } else {
        r.line(Level::Ok, "no se detectó firewalld ni ufw");
    }

    // --- Bluetooth ------------------------------------------------------------
    r.section("Bluetooth");
    match bluer::Session::new().await {
        Ok(session) => match session.default_adapter().await {
            Ok(adapter) => {
                let address = adapter
                    .address()
                    .await
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                if adapter.is_powered().await.unwrap_or(false) {
                    r.line(
                        Level::Ok,
                        format!("adaptador {} ({address}) encendido", adapter.name()),
                    );
                } else {
                    r.line(
                        Level::Warn,
                        format!(
                            "adaptador {} apagado: sin enlace por Bluetooth",
                            adapter.name()
                        ),
                    );
                    r.fix("bluetoothctl power on");
                }
            }
            Err(_) => r.line(
                Level::Warn,
                "no hay adaptador Bluetooth: el enlace solo funcionará por Wi-Fi",
            ),
        },
        Err(_) => {
            r.line(Level::Warn, "BlueZ (bluetoothd) no responde");
            r.fix("sudo systemctl enable --now bluetooth");
        }
    }

    // --- Notificaciones -------------------------------------------------------
    r.section("Notificaciones");
    match kushare_tray::Notifier::connect().await {
        Ok(notifier) => {
            let caps = notifier.capabilities().await;
            if caps.is_empty() {
                r.line(
                    Level::Bad,
                    "no hay servidor de notificaciones (org.freedesktop.Notifications)",
                );
            } else if caps.iter().any(|c| c == "inline-reply") {
                r.line(Level::Ok, "servidor con respuesta en línea (inline-reply)");
            } else if which("zenity") || which("kdialog") {
                r.line(
                    Level::Ok,
                    "servidor sin inline-reply: se responderá con un diálogo (zenity/kdialog)",
                );
            } else {
                r.line(Level::Warn, "servidor sin inline-reply y sin zenity ni kdialog: no se podrá responder desde el PC");
                r.fix("instala zenity (o kdialog)");
            }
        }
        Err(_) => r.line(Level::Bad, "no se pudo conectar al bus de sesión D-Bus"),
    }
    for (process, name) in [("kdeconnectd", "KDE Connect"), ("gsconnect", "GSConnect")] {
        if run("pgrep", &["-x", process]).is_some() {
            r.line(
                Level::Warn,
                format!("{name} está corriendo: si también sincroniza notificaciones, las verás duplicadas"),
            );
            r.fix(format!("desactiva el plugin de notificaciones de {name}"));
        }
    }

    r.lines.push(String::new());
    r.lines.push(if r.problems == 0 {
        "Todo en orden.".to_string()
    } else {
        format!(
            "{} aviso(s). Más ayuda: FAQ.md (/usr/share/doc/kushare/FAQ.md)",
            r.problems
        )
    });
    println!("{}", r.lines.join("\n"));
    Ok(())
}

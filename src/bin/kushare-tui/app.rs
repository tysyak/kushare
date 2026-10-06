use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kushare_core::{DiscoveredDevice, Settings, TransferEvent, random_endpoint_id};
use kushare_discovery::mdns::{MdnsAdvertiser, MdnsScanner, discovered_device_from_event};
use kushare_transfer::{Receiver, Sender};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

const LOG_CAPACITY: usize = 200;

/// Eventos que las tareas de fondo (mDNS, recepción, envío) empujan hacia
/// el loop principal de la TUI.
pub enum AppEvent {
    DeviceDiscovered(DiscoveredDevice),
    /// Eventos del lado de recepción (pantalla "Recibir"): se muestran en
    /// su log.
    Transfer(TransferEvent),
    /// Progreso del envío en curso (pantalla "Enviar"): separado de
    /// `Transfer` a propósito, para no confundir el progreso de un envío
    /// propio con el de una recepción que pudiera estar ocurriendo a la
    /// vez en segundo plano.
    SendProgress {
        bytes: u64,
        total: u64,
    },
    SendResult(Result<(), String>),
    Status(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HomeChoice {
    Enviar,
    Recibir,
}

pub struct ReceiveScreen {
    pub name_input: String,
    pub editing_name: bool,
    pub visible: bool,
    pub log: VecDeque<String>,
}

pub enum SendStatus {
    PickingFile,
    PickingDevice,
    Sending { bytes: u64, total: u64 },
    Done,
    Failed(String),
}

pub struct SendScreen {
    pub file: Option<PathBuf>,
    pub devices: Vec<DiscoveredDevice>,
    pub selected: usize,
    pub status: SendStatus,
    pub path_input: String,
    pub editing_path: bool,
    pub yazi_available: bool,
    scanning_started: bool,
}

pub enum Screen {
    Home { selected: HomeChoice },
    Receive(ReceiveScreen),
    Send(SendScreen),
}

pub struct App {
    pub settings: Settings,
    pub endpoint_id: String,
    pub screen: Screen,
    pub status_line: String,
    tx: mpsc::UnboundedSender<AppEvent>,
    receiver_started: bool,
    advertiser: Option<MdnsAdvertiser>,
}

impl App {
    pub fn new(settings: Settings, tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            endpoint_id: random_endpoint_id(),
            screen: Screen::Home {
                selected: HomeChoice::Enviar,
            },
            status_line: String::new(),
            settings,
            tx,
            receiver_started: false,
            advertiser: None,
        }
    }

    /// Procesa una tecla. Devuelve `false` cuando la app debe salir.
    pub async fn handle_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> bool {
        // Ctrl+C sale desde cualquier pantalla.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }

        match &mut self.screen {
            Screen::Home { selected } => match key.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    *selected = match selected {
                        HomeChoice::Enviar => HomeChoice::Recibir,
                        HomeChoice::Recibir => HomeChoice::Enviar,
                    };
                }
                KeyCode::Enter => {
                    let choice = *selected;
                    match choice {
                        HomeChoice::Enviar => self.enter_send(None).await,
                        HomeChoice::Recibir => self.enter_receive().await,
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc => return false,
                _ => {}
            },
            Screen::Receive(_) => self.handle_receive_key(key).await,
            Screen::Send(_) => self.handle_send_key(key, terminal).await,
        }

        true
    }

    pub fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::DeviceDiscovered(device) => {
                if let Screen::Send(send) = &mut self.screen
                    && !send
                        .devices
                        .iter()
                        .any(|d| d.endpoint_id == device.endpoint_id)
                {
                    send.devices.push(device);
                }
            }
            AppEvent::Transfer(transfer_event) => {
                let line = format_transfer_event(&transfer_event);
                if let Screen::Receive(receive) = &mut self.screen {
                    push_log(&mut receive.log, line);
                } else {
                    self.status_line = line;
                }
            }
            AppEvent::SendProgress { bytes, total } => self.apply_send_progress(bytes, total),
            AppEvent::SendResult(result) => {
                if let Screen::Send(send) = &mut self.screen {
                    send.status = match result {
                        Ok(()) => SendStatus::Done,
                        Err(reason) => SendStatus::Failed(reason),
                    };
                }
            }
            AppEvent::Status(text) => self.status_line = text,
        }
    }

    // ---------------------------------------------------------------
    // Pantalla "Recibir"
    // ---------------------------------------------------------------

    pub async fn enter_receive(&mut self) {
        if !self.receiver_started {
            self.receiver_started = true;
            match Receiver::bind(
                self.settings.listen_port,
                self.settings.download_dir.clone(),
                self.receiver_events_tx(),
            )
            .await
            {
                Ok(receiver) => {
                    if let Ok(addr) = receiver.local_addr() {
                        self.settings.listen_port = addr.port();
                    }
                    tokio::spawn(receiver.run());
                }
                Err(err) => {
                    self.status_line = format!("no se pudo escuchar: {err}");
                }
            }
        }

        let visible = self.start_advertising();
        self.screen = Screen::Receive(ReceiveScreen {
            name_input: self.settings.device_name.clone(),
            editing_name: false,
            visible,
            log: VecDeque::new(),
        });
    }

    /// Puente entre el `mpsc::Sender<TransferEvent>` que espera
    /// `kushare_transfer::Receiver` y el canal unificado `AppEvent` de la
    /// TUI.
    fn receiver_events_tx(&self) -> mpsc::Sender<TransferEvent> {
        let (tx, mut rx) = mpsc::channel::<TransferEvent>(64);
        let app_tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = app_tx.send(AppEvent::Transfer(event));
            }
        });
        tx
    }

    fn start_advertising(&mut self) -> bool {
        match MdnsAdvertiser::new() {
            Ok(mut advertiser) => {
                if let Err(err) = advertiser.start_advertising(
                    &self.settings.device_name,
                    &self.endpoint_id,
                    self.settings.listen_port,
                ) {
                    self.status_line = format!("no se pudo anunciar por mDNS: {err}");
                    return false;
                }
                self.advertiser = Some(advertiser);
                true
            }
            Err(err) => {
                self.status_line = format!("mDNS no disponible: {err}");
                false
            }
        }
    }

    fn stop_advertising(&mut self) {
        if let Some(advertiser) = &mut self.advertiser {
            let _ = advertiser.stop_advertising();
        }
        self.advertiser = None;
    }

    async fn handle_receive_key(&mut self, key: KeyEvent) {
        let Screen::Receive(receive) = &mut self.screen else {
            return;
        };

        if receive.editing_name {
            match key.code {
                KeyCode::Enter => {
                    receive.editing_name = false;
                    if !receive.name_input.trim().is_empty() {
                        self.settings.device_name = receive.name_input.trim().to_string();
                        if receive.visible {
                            self.stop_advertising();
                            let visible = self.start_advertising();
                            if let Screen::Receive(receive) = &mut self.screen {
                                receive.visible = visible;
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    receive.editing_name = false;
                    receive.name_input = self.settings.device_name.clone();
                }
                KeyCode::Backspace => {
                    receive.name_input.pop();
                }
                KeyCode::Char(c) => receive.name_input.push(c),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('e') => receive.editing_name = true,
            KeyCode::Char('v') => {
                if receive.visible {
                    self.stop_advertising();
                    if let Screen::Receive(receive) = &mut self.screen {
                        receive.visible = false;
                    }
                } else {
                    let visible = self.start_advertising();
                    if let Screen::Receive(receive) = &mut self.screen {
                        receive.visible = visible;
                    }
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                self.stop_advertising();
                self.screen = Screen::Home {
                    selected: HomeChoice::Recibir,
                };
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------
    // Pantalla "Enviar"
    // ---------------------------------------------------------------

    pub async fn enter_send(&mut self, file: Option<PathBuf>) {
        let status = if file.is_some() {
            SendStatus::PickingDevice
        } else {
            SendStatus::PickingFile
        };
        self.screen = Screen::Send(SendScreen {
            file,
            devices: Vec::new(),
            selected: 0,
            status,
            path_input: String::new(),
            editing_path: false,
            yazi_available: which_yazi(),
            scanning_started: false,
        });
        self.maybe_start_scanning();
    }

    fn maybe_start_scanning(&mut self) {
        let Screen::Send(send) = &mut self.screen else {
            return;
        };
        if send.file.is_none() || send.scanning_started {
            return;
        }
        send.scanning_started = true;

        let app_tx = self.tx.clone();
        match MdnsScanner::new() {
            Ok(scanner) => match scanner.browse() {
                Ok(mdns_events) => {
                    tokio::spawn(async move {
                        let _scanner = scanner; // mantener vivo el daemon mientras se escanea
                        while let Ok(event) = mdns_events.recv_async().await {
                            if let Some(device) = discovered_device_from_event(&event) {
                                let _ = app_tx.send(AppEvent::DeviceDiscovered(device));
                            }
                        }
                    });
                }
                Err(err) => {
                    let _ = app_tx.send(AppEvent::Status(format!(
                        "no se pudo escanear por mDNS: {err}"
                    )));
                }
            },
            Err(err) => {
                let _ = app_tx.send(AppEvent::Status(format!("mDNS no disponible: {err}")));
            }
        }
    }

    async fn handle_send_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) {
        let Screen::Send(send) = &mut self.screen else {
            return;
        };

        if send.editing_path {
            match key.code {
                KeyCode::Enter => {
                    let path = PathBuf::from(send.path_input.trim());
                    send.editing_path = false;
                    if path.as_os_str().is_empty() {
                        return;
                    }
                    send.file = Some(path);
                    send.status = SendStatus::PickingDevice;
                    self.maybe_start_scanning();
                }
                KeyCode::Esc => send.editing_path = false,
                KeyCode::Backspace => {
                    send.path_input.pop();
                }
                KeyCode::Char(c) => send.path_input.push(c),
                _ => {}
            }
            return;
        }

        match &send.status {
            SendStatus::PickingFile => match key.code {
                KeyCode::Char('y') if send.yazi_available => {
                    if let Some(path) = pick_file_with_yazi(terminal).await {
                        send.file = Some(path);
                        send.status = SendStatus::PickingDevice;
                        self.maybe_start_scanning();
                    }
                }
                KeyCode::Char('p') => {
                    send.editing_path = true;
                    send.path_input.clear();
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.screen = Screen::Home {
                        selected: HomeChoice::Enviar,
                    };
                }
                _ => {}
            },
            SendStatus::PickingDevice => match key.code {
                KeyCode::Up => {
                    if send.selected > 0 {
                        send.selected -= 1;
                    }
                }
                KeyCode::Down => {
                    if send.selected + 1 < send.devices.len() {
                        send.selected += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(device) = send.devices.get(send.selected).cloned() {
                        let path = send.file.clone().expect("hay archivo al llegar aquí");
                        send.status = SendStatus::Sending { bytes: 0, total: 0 };
                        self.spawn_send(device, path);
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.screen = Screen::Home {
                        selected: HomeChoice::Enviar,
                    };
                }
                _ => {}
            },
            SendStatus::Sending { .. } => {
                // Sin acciones mientras se envía; esperar a que termine.
            }
            SendStatus::Done | SendStatus::Failed(_) => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                    self.screen = Screen::Home {
                        selected: HomeChoice::Enviar,
                    };
                }
            }
        }
    }

    fn spawn_send(&self, device: DiscoveredDevice, path: PathBuf) {
        let app_tx = self.tx.clone();
        let endpoint_id = self.endpoint_id.clone();
        let device_name = self.settings.device_name.clone();
        let (events_tx, mut events_rx) = mpsc::channel::<TransferEvent>(32);

        let progress_tx = app_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if let TransferEvent::Progress {
                    bytes_transferred,
                    total_bytes,
                    ..
                } = event
                {
                    let _ = progress_tx.send(AppEvent::SendProgress {
                        bytes: bytes_transferred,
                        total: total_bytes,
                    });
                }
            }
        });

        tokio::spawn(async move {
            let result = async {
                let mut session = Sender::connect(device.addr).await?;
                Sender::send_file(&mut session, &endpoint_id, &device_name, &path, events_tx).await
            }
            .await;
            let _ = app_tx.send(AppEvent::SendResult(result.map_err(|e| e.to_string())));
        });
    }
}

fn push_log(log: &mut VecDeque<String>, line: String) {
    log.push_back(line);
    while log.len() > LOG_CAPACITY {
        log.pop_front();
    }
}

fn format_transfer_event(event: &TransferEvent) -> String {
    match event {
        TransferEvent::IncomingRequest {
            device_name,
            file_name,
            file_size,
        } => format!("↓ {device_name} quiere enviar \"{file_name}\" ({file_size} bytes)"),
        TransferEvent::Progress {
            bytes_transferred,
            total_bytes,
            ..
        } => {
            let pct = if *total_bytes > 0 {
                (*bytes_transferred as f64 / *total_bytes as f64) * 100.0
            } else {
                0.0
            };
            format!("… {bytes_transferred}/{total_bytes} bytes ({pct:.0}%)")
        }
        TransferEvent::Completed {
            device_name,
            file_name,
            saved_to,
        } => format!(
            "✓ recibido \"{file_name}\" de {device_name} -> {}",
            saved_to.display()
        ),
        TransferEvent::Failed { file_name, reason } => {
            format!("✗ error con \"{file_name}\": {reason}")
        }
    }
}

impl App {
    fn apply_send_progress(&mut self, bytes: u64, total: u64) {
        if let Screen::Send(send) = &mut self.screen
            && matches!(send.status, SendStatus::Sending { .. })
        {
            send.status = SendStatus::Sending { bytes, total };
        }
    }
}

fn which_yazi() -> bool {
    std::process::Command::new("which")
        .arg("yazi")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Suspende la TUI, abre `yazi --chooser-file <tmp>` para elegir un
/// archivo, y restaura la TUI al volver. Devuelve `None` si el usuario
/// salió de yazi sin elegir nada o si yazi falló.
async fn pick_file_with_yazi(terminal: &mut DefaultTerminal) -> Option<PathBuf> {
    use crossterm::execute;
    use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    let tmp = tempfile::NamedTempFile::new().ok()?;
    let status = tokio::process::Command::new("yazi")
        .arg("--chooser-file")
        .arg(tmp.path())
        .status()
        .await
        .ok()?;

    let _ = execute!(
        terminal.backend_mut(),
        crossterm::terminal::EnterAlternateScreen
    );
    let _ = enable_raw_mode();
    let _ = terminal.clear();

    if !status.success() {
        return None;
    }

    let content = tokio::fs::read_to_string(tmp.path()).await.ok()?;
    content
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

//! Tarjeta *Compartir internet* (Ajustes): el PC crea una red Wi-Fi para el
//! teléfono. Se elige el nombre y la clave (o se genera una al azar); con la
//! red encendida se muestra el QR para conectarse con la cámara y un botón
//! para mandarla al teléfono por el enlace, sin escanear.

use super::*;
use iced::widget::{column, qr_code, text_input};
use kushare_dbus::{HotspotRow, UsbNetRow};

#[derive(Debug, Default)]
pub struct HotspotState {
    pub row: HotspotRow,
    /// Lo que se está escribiendo (se guarda con «Guardar»).
    pub ssid: String,
    pub password: String,
    pub show_password: bool,
    /// Hay cambios sin guardar en los campos.
    pub dirty: bool,
    pub qr: Option<qr_code::Data>,
    /// Esperando a que encienda (polkit + hostapd).
    pub starting: bool,
    /// Texto del botón «Usar la antena del canal N…» (vacío si no hay).
    pub switch_label: String,
    /// Internet por cable USB.
    pub usb: UsbNetRow,
    pub usb_starting: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    Loaded(HotspotRow, String),
    Toggle(bool),
    Ssid(String),
    Password(String),
    ShowPassword,
    Generate,
    Generated(String),
    Save,
    Saved(Result<(), String>),
    Send,
    /// Pasar la Wi-Fi del PC a la antena compatible y encender.
    SwitchAntenna,
    UsbLoaded(UsbNetRow),
    UsbToggle(bool),
}

impl App {
    pub(crate) fn hotspot_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let phone = self.phone_peers().first().map(|p| p.id.clone());
        let s = &mut self.hotspot;
        match msg {
            Msg::Load => Task::batch([
                load(d.clone()),
                Task::perform(
                    async move { d.usbnet_status().await.unwrap_or_default() },
                    |r| Message::Hotspot(Msg::UsbLoaded(r)),
                ),
            ]),
            Msg::UsbLoaded(row) => {
                if s.usb_starting && !row.1 {
                    s.usb_starting = false;
                    self.toast = Some(if row.0 {
                        t("gui.hotspot.usb_started").to_string()
                    } else if row.4.is_empty() {
                        t("gui.hotspot.usb_failed").to_string()
                    } else {
                        row.4.clone()
                    });
                }
                s.usb = row;
                Task::none()
            }
            Msg::UsbToggle(on) => {
                if on {
                    let Some(peer) = phone else {
                        return Task::none();
                    };
                    s.usb_starting = true;
                    s.usb.1 = true;
                    self.toast = Some(t("gui.hotspot.usb_wait").to_string());
                    Task::perform(
                        async move { d.usbnet_start(&peer).await.map_err(err_text) },
                        |r| match r {
                            Ok(()) => Message::Hotspot(Msg::Load),
                            Err(e) => Message::Hotspot(Msg::Saved(Err(e))),
                        },
                    )
                } else {
                    Task::perform(
                        async move { d.usbnet_stop().await.map_err(err_text) },
                        |_| Message::Hotspot(Msg::Load),
                    )
                }
            }
            Msg::Loaded(row, qr) => {
                let was_active = s.row.0;
                if !s.dirty {
                    s.ssid = row.2.clone();
                    s.password = row.3.clone();
                }
                s.qr = row.0.then(|| qr_code::Data::new(qr).ok()).flatten();
                s.switch_label = tf("gui.hotspot.switch", &[("alternative", &row.7)]);
                // Avisar del resultado de encenderla.
                let failed = s.starting && !row.1 && !row.0 && !row.6.is_empty();
                if !row.1 {
                    s.starting = false;
                }
                if failed {
                    self.toast = Some(row.6.clone());
                } else if !was_active && row.0 {
                    self.toast = Some(t("gui.hotspot.started").to_string());
                }
                s.row = row;
                Task::none()
            }
            Msg::Toggle(on) => {
                if on {
                    s.starting = true;
                    s.row.1 = true;
                    // Guardar antes lo escrito, para encenderla con eso.
                    let save = s.dirty.then(|| (s.ssid.clone(), s.password.clone()));
                    s.dirty = false;
                    Task::perform(
                        async move {
                            if let Some((ssid, password)) = save {
                                d.hotspot_set_config(&ssid, &password)
                                    .await
                                    .map_err(err_text)?;
                            }
                            d.hotspot_start().await.map_err(err_text)
                        },
                        |r: Result<(), String>| match r {
                            Ok(()) => Message::Hotspot(Msg::Load),
                            Err(e) => Message::Hotspot(Msg::Saved(Err(e))),
                        },
                    )
                } else {
                    Task::perform(
                        async move { d.hotspot_stop().await.map_err(err_text) },
                        |_| Message::Hotspot(Msg::Load),
                    )
                }
            }
            Msg::Ssid(v) => {
                s.ssid = v;
                s.dirty = true;
                Task::none()
            }
            Msg::Password(v) => {
                s.password = v;
                s.dirty = true;
                Task::none()
            }
            Msg::ShowPassword => {
                s.show_password = !s.show_password;
                Task::none()
            }
            Msg::Generate => Task::perform(
                async move { d.hotspot_random_password().await.unwrap_or_default() },
                |p| Message::Hotspot(Msg::Generated(p)),
            ),
            Msg::Generated(p) => {
                if !p.is_empty() {
                    s.password = p;
                    s.show_password = true;
                    s.dirty = true;
                }
                Task::none()
            }
            Msg::Save => {
                let (ssid, password) = (s.ssid.clone(), s.password.clone());
                Task::perform(
                    async move {
                        d.hotspot_set_config(&ssid, &password)
                            .await
                            .map_err(err_text)
                    },
                    |r| Message::Hotspot(Msg::Saved(r)),
                )
            }
            Msg::Saved(result) => {
                s.starting = false;
                match result {
                    Ok(()) => {
                        s.dirty = false;
                        self.toast = Some(t("gui.hotspot.saved").to_string());
                    }
                    Err(e) => self.toast = Some(e),
                }
                load(d)
            }
            Msg::SwitchAntenna => {
                s.starting = true;
                s.row.1 = true;
                self.toast = Some(t("gui.hotspot.switching").to_string());
                Task::perform(
                    async move { d.hotspot_switch_antenna().await.map_err(err_text) },
                    |r| match r {
                        Ok(()) => Message::Hotspot(Msg::Load),
                        Err(e) => Message::Hotspot(Msg::Saved(Err(e))),
                    },
                )
            }
            Msg::Send => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                self.call(t("gui.hotspot.sent"), move |d| async move {
                    d.hotspot_send(&peer).await
                })
            }
        }
    }

    pub(crate) fn hotspot_card(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.hotspot;
        let (active, starting, _, _, band, clients, error, alternative) = &s.row;
        let busy = *starting || s.starting;
        let status = if *active {
            tf(
                "gui.hotspot.on",
                &[("band", band), ("clients", &clients.to_string())],
            )
        } else if busy {
            t("gui.hotspot.starting").to_string()
        } else if !error.is_empty() {
            error.clone()
        } else {
            t("gui.hotspot.detail").to_string()
        };
        let mut col = column![
            self.switch_row(c, t("gui.hotspot.title"), None, *active || busy, |on| {
                Message::Hotspot(Msg::Toggle(on))
            },),
            text(status)
                .size(13)
                .color(if !*active && !busy && !error.is_empty() {
                    c.error
                } else {
                    c.on_surface_variant
                }),
        ]
        .spacing(8);

        // El canal de la Wi-Fi del PC no sirve, pero hay otra antena que sí.
        if !*active && !busy && !alternative.is_empty() {
            col = col.push(
                button(labeled(Icon::Refresh, &s.switch_label, c.on_primary))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::Hotspot(Msg::SwitchAntenna)),
            );
            col = col.push(
                text(t("gui.hotspot.switch_detail"))
                    .size(12)
                    .color(c.on_surface_variant),
            );
        }

        // Nombre y clave: solo se editan con la red apagada.
        let editable = !*active && !busy;
        let field = |label: &'static str| text(t(label)).size(13).font(SEMIBOLD);
        let ssid = text_input(t("gui.hotspot.ssid"), &s.ssid)
            .on_input_maybe(editable.then_some(|v| Message::Hotspot(Msg::Ssid(v))))
            .padding(10)
            .width(Length::Fill);
        let password = text_input(t("gui.hotspot.password"), &s.password)
            .secure(!s.show_password)
            .on_input_maybe(editable.then_some(|v| Message::Hotspot(Msg::Password(v))))
            .padding(10)
            .width(Length::Fill);
        let eye = button(
            text(if s.show_password {
                t("gui.hotspot.hide")
            } else {
                t("gui.hotspot.show")
            })
            .size(13),
        )
        .style(style::button(c, ButtonKind::Text))
        .on_press(Message::Hotspot(Msg::ShowPassword));
        let generate = button(labeled(
            Icon::Refresh,
            t("gui.hotspot.generate"),
            c.on_surface,
        ))
        .style(style::button(c, ButtonKind::Tonal))
        .on_press_maybe(editable.then_some(Message::Hotspot(Msg::Generate)));
        let save = button(labeled(Icon::Save, t("gui.hotspot.save"), c.on_primary))
            .style(style::button(c, ButtonKind::Filled))
            .on_press_maybe((editable && s.dirty).then_some(Message::Hotspot(Msg::Save)));
        col = col.push(
            column![
                field("gui.hotspot.ssid"),
                ssid,
                field("gui.hotspot.password"),
                row![password, eye].spacing(8).align_y(Alignment::Center),
                row![generate, save].spacing(8),
            ]
            .spacing(6),
        );

        // Por cable USB (avanzado).
        let (usb_on, usb_busy, rx, tx, usb_error) = &s.usb;
        let usb_status = if *usb_on {
            tf(
                "gui.hotspot.usb_on",
                &[("down", &human_bytes(*rx)), ("up", &human_bytes(*tx))],
            )
        } else if *usb_busy || s.usb_starting {
            t("gui.hotspot.usb_wait").to_string()
        } else if !usb_error.is_empty() {
            usb_error.clone()
        } else {
            t("gui.hotspot.usb_detail").to_string()
        };
        let usb_col = column![
            self.switch_row(
                c,
                t("gui.hotspot.usb_title"),
                None,
                *usb_on || *usb_busy || s.usb_starting,
                |on| Message::Hotspot(Msg::UsbToggle(on)),
            ),
            text(usb_status)
                .size(13)
                .color(if !*usb_on && !usb_error.is_empty() {
                    c.error
                } else {
                    c.on_surface_variant
                }),
        ]
        .spacing(6);

        if *active {
            let qr: Element<'_, Message> = match &s.qr {
                Some(data) => container(qr_code(data).cell_size(5).style(|_| {
                    // Negro sobre blanco en cualquier tema: las cámaras no
                    // leen bien un QR invertido.
                    iced::widget::qr_code::Style {
                        cell: Color::BLACK,
                        background: Color::WHITE,
                    }
                }))
                .padding(10)
                .style(|_| container::Style {
                    background: Some(Color::WHITE.into()),
                    border: iced::border::rounded(12),
                    ..Default::default()
                })
                .into(),
                None => text("").into(),
            };
            let send = button(labeled(Icon::Phone, t("gui.hotspot.send"), c.on_primary))
                .style(style::button(c, ButtonKind::Filled))
                .on_press_maybe(
                    (!self.phone_peers().is_empty()).then_some(Message::Hotspot(Msg::Send)),
                );
            col = col.push(
                row![
                    qr,
                    column![
                        text(t("gui.hotspot.scan")).size(14),
                        text(format!("{} · {}", s.ssid, s.password))
                            .size(13)
                            .font(SEMIBOLD),
                        send,
                    ]
                    .spacing(10)
                    .width(Length::Fill),
                ]
                .spacing(16)
                .align_y(Alignment::Center),
            );
        }
        col.push(usb_col).into()
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KiB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn load(d: DaemonProxy<'static>) -> Task<Message> {
    Task::perform(
        async move {
            let row = d.hotspot_status().await.unwrap_or_default();
            let qr = d.hotspot_qr().await.unwrap_or_default();
            (row, qr)
        },
        |(row, qr)| Message::Hotspot(Msg::Loaded(row, qr)),
    )
}

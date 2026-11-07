//! Visor de fotos propio: al pulsar una miniatura se agranda al instante
//! y, en cuanto la original está descargada (o ya estaba precargada), la
//! sustituye. Desde aquí: abrir con el visor del sistema, copiar, guardar
//! como… y eliminar del teléfono. ← / → para pasar, Esc para cerrar.

use super::*;
use iced::widget::column;

#[derive(Debug, Clone)]
pub struct Viewer {
    pub peer: String,
    pub index: usize,
    pub id: String,
    /// Ruta de la original (cuando ya está).
    pub original: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub confirm_delete: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Open(usize),
    Loaded(String, Result<String, String>),
    Prev,
    Next,
    Close,
    External,
    Copy,
    SaveAs,
    SaveTo(Option<PathBuf>),
    AskDelete,
    CancelDelete,
    Delete,
    Deleted(String, Result<String, String>),
}

impl App {
    pub(crate) fn viewer_update(&mut self, msg: Msg) -> Task<Message> {
        match msg {
            Msg::Open(index) => {
                let Some(photo) = self.gallery.photos.get(index) else {
                    return Task::none();
                };
                let original = (!photo.7.is_empty() && std::path::Path::new(&photo.7).exists())
                    .then(|| photo.7.clone());
                let id = photo.0.clone();
                let loading = original.is_none();
                self.viewer = Some(Viewer {
                    peer: self.gallery.peer.clone(),
                    index,
                    id: id.clone(),
                    original,
                    loading,
                    error: None,
                    confirm_delete: false,
                });
                if !loading {
                    return Task::none();
                }
                let (Some(d), peer) = (self.daemon.clone(), self.gallery.peer.clone()) else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        (
                            id.clone(),
                            d.fetch_photo(&peer, &id).await.map_err(err_text),
                        )
                    },
                    |(id, r)| Message::Viewer(Msg::Loaded(id, r)),
                )
            }
            Msg::Loaded(id, result) => {
                if let Ok(path) = &result
                    && let Some(p) = self.gallery.photos.iter_mut().find(|p| p.0 == id)
                {
                    p.7 = path.clone();
                }
                if let Some(v) = &mut self.viewer
                    && v.id == id
                {
                    v.loading = false;
                    match result {
                        Ok(path) => v.original = Some(path),
                        Err(e) => v.error = Some(e),
                    }
                }
                Task::none()
            }
            Msg::Prev | Msg::Next => {
                let Some(v) = &self.viewer else {
                    return Task::none();
                };
                let len = self.gallery.photos.len();
                let next = match msg {
                    Msg::Prev if v.index > 0 => v.index - 1,
                    Msg::Next if v.index + 1 < len => v.index + 1,
                    _ => return Task::none(),
                };
                Task::done(Message::Viewer(Msg::Open(next)))
            }
            Msg::Close => {
                self.viewer = None;
                Task::none()
            }
            Msg::External => {
                if let Some(path) = self.viewer.as_ref().and_then(|v| v.original.clone()) {
                    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
                }
                Task::none()
            }
            Msg::Copy => {
                let Some(path) = self.viewer.as_ref().and_then(|v| v.original.clone()) else {
                    return Task::none();
                };
                self.call(t("gui.viewer.copied"), move |d| async move {
                    d.clipboard_copy_file(&path).await
                })
            }
            Msg::SaveAs => {
                let Some(v) = &self.viewer else {
                    return Task::none();
                };
                let name = self
                    .gallery
                    .photos
                    .get(v.index)
                    .map(|p| p.1.clone())
                    .unwrap_or_default();
                Task::perform(
                    async move {
                        let mut dialog = rfd::AsyncFileDialog::new().set_file_name(name);
                        if let Some(dir) = directories::UserDirs::new()
                            .and_then(|d| d.picture_dir().map(PathBuf::from))
                        {
                            dialog = dialog.set_directory(dir);
                        }
                        dialog.save_file().await.map(|f| f.path().to_path_buf())
                    },
                    |p| Message::Viewer(Msg::SaveTo(p)),
                )
            }
            Msg::SaveTo(None) => Task::none(),
            Msg::SaveTo(Some(dest)) => {
                let Some(v) = &self.viewer else {
                    return Task::none();
                };
                let (Some(d), peer, id) = (self.daemon.clone(), v.peer.clone(), v.id.clone())
                else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        d.save_photo(&peer, &id, &dest.to_string_lossy())
                            .await
                            .map(|p| tf("gui.photos.saved", &[("path", &p)]))
                            .map_err(err_text)
                    },
                    Message::Done,
                )
            }
            Msg::AskDelete => {
                if let Some(v) = &mut self.viewer {
                    v.confirm_delete = true;
                }
                Task::none()
            }
            Msg::CancelDelete => {
                if let Some(v) = &mut self.viewer {
                    v.confirm_delete = false;
                }
                Task::none()
            }
            Msg::Delete => {
                let Some(v) = &mut self.viewer else {
                    return Task::none();
                };
                v.confirm_delete = false;
                let (Some(d), peer, id) = (self.daemon.clone(), v.peer.clone(), v.id.clone())
                else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        (
                            id.clone(),
                            d.delete_photo(&peer, &id).await.map_err(err_text),
                        )
                    },
                    |(id, r)| Message::Viewer(Msg::Deleted(id, r)),
                )
            }
            Msg::Deleted(id, result) => {
                match result {
                    Ok(status) if status == "done" => {
                        let pos = self.gallery.photos.iter().position(|p| p.0 == id);
                        if let Some(pos) = pos {
                            self.gallery.photos.remove(pos);
                            self.gallery.total = self.gallery.total.saturating_sub(1);
                        }
                        self.toast = Some(t("gui.viewer.deleted").into());
                        // Pasar a la siguiente (o cerrar si no quedan).
                        match pos {
                            Some(pos) if pos < self.gallery.photos.len() => {
                                return Task::done(Message::Viewer(Msg::Open(pos)));
                            }
                            Some(pos) if pos > 0 => {
                                return Task::done(Message::Viewer(Msg::Open(pos - 1)));
                            }
                            _ => self.viewer = None,
                        }
                    }
                    Ok(_) => self.toast = Some(t("gui.viewer.confirm_on_phone").into()),
                    Err(e) => self.toast = Some(e),
                }
                Task::none()
            }
        }
    }

    pub(crate) fn viewer_view<'a>(&'a self, c: Colors, v: &'a Viewer) -> Element<'a, Message> {
        let photo = self.gallery.photos.get(v.index);
        let (name, info) = photo
            .map(|p| {
                (
                    p.1.clone(),
                    format!("{} · {}×{} · {}", fmt_time(p.2), p.3, p.4, fmt_size(p.5)),
                )
            })
            .unwrap_or_default();
        let source = v
            .original
            .clone()
            .or_else(|| photo.map(|p| p.6.clone()))
            .unwrap_or_default();
        let picture = iced::widget::image(iced::widget::image::Handle::from_path(source))
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(iced::ContentFit::Contain);
        let white = Color::WHITE;
        let ghost = |icon: Icon, msg: Option<Message>| {
            button(icon.view(26.0, white))
                .padding(12)
                .style(style::button(
                    c,
                    ButtonKind::Card {
                        bg: Color {
                            a: 0.35,
                            ..Color::BLACK
                        },
                        fg: white,
                        shape: Shape::List,
                    },
                ))
                .on_press_maybe(msg)
        };
        let status: Element<'a, Message> = if v.loading {
            row![
                Icon::Download.view(16.0, white),
                text(t("gui.viewer.loading")).size(13).color(white)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else if let Some(err) = &v.error {
            text(err.clone()).size(13).color(c.error).into()
        } else {
            space().into()
        };
        let top = row![
            column![
                text(name).size(18).font(BOLD).color(white),
                text(info).size(13).color(Color { a: 0.8, ..white }),
            ]
            .width(Length::Fill),
            status,
            ghost(Icon::Close, Some(Message::Viewer(Msg::Close))),
        ]
        .spacing(14)
        .align_y(Alignment::Center);

        let has = v.original.is_some();
        let action = |icon: Icon, label: &'a str, msg: Option<Message>| {
            button(labeled(icon, label, white))
                .padding(Padding::from([10, 18]))
                .style(style::button(
                    c,
                    ButtonKind::Card {
                        bg: Color {
                            a: 0.45,
                            ..Color::BLACK
                        },
                        fg: white,
                        shape: Shape::List,
                    },
                ))
                .on_press_maybe(msg)
        };
        let bottom: Element<'a, Message> = if v.confirm_delete {
            row![
                text(t("gui.viewer.delete_question"))
                    .color(white)
                    .font(SEMIBOLD),
                space().width(Length::Fill),
                action(
                    Icon::Close,
                    t("gui.common.cancel"),
                    Some(Message::Viewer(Msg::CancelDelete))
                ),
                button(labeled(Icon::Delete, t("gui.viewer.delete"), c.on_primary))
                    .padding(Padding::from([10, 18]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::Viewer(Msg::Delete)),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                space().width(Length::Fill),
                action(
                    Icon::OpenInNew,
                    t("gui.viewer.open"),
                    has.then_some(Message::Viewer(Msg::External))
                ),
                action(
                    Icon::Copy,
                    t("gui.viewer.copy"),
                    has.then_some(Message::Viewer(Msg::Copy))
                ),
                action(
                    Icon::Save,
                    t("gui.viewer.save_as"),
                    has.then_some(Message::Viewer(Msg::SaveAs))
                ),
                action(
                    Icon::Delete,
                    t("gui.viewer.delete"),
                    Some(Message::Viewer(Msg::AskDelete))
                ),
                space().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        };
        let middle = row![
            ghost(
                Icon::ChevronLeft,
                (v.index > 0).then_some(Message::Viewer(Msg::Prev))
            ),
            container(picture).width(Length::Fill).height(Length::Fill),
            ghost(
                Icon::ChevronRight,
                (v.index + 1 < self.gallery.photos.len()).then_some(Message::Viewer(Msg::Next))
            ),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .height(Length::Fill);
        opaque(
            container(column![top, middle, bottom].spacing(16))
                .padding(24)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_: &Theme| container::Style {
                    background: Some(
                        Color {
                            a: 0.92,
                            ..Color::BLACK
                        }
                        .into(),
                    ),
                    ..Default::default()
                }),
        )
    }
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

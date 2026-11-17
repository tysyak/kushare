//! Página Fotos: galería del teléfono y su barra de herramientas.

use super::*;
use iced::widget::column;

impl App {
    pub(crate) fn photos(&self, c: Colors) -> Element<'_, Message> {
        let phones = self.phone_peers();
        let g = &self.gallery;
        let subtitle = if g.total > 0 {
            tf(
                "gui.photos.count",
                &[("shown", &g.photos.len()), ("total", &g.total)],
            )
        } else {
            t("gui.photos.subtitle").into()
        };
        let mut col = column![self.title(c, t("gui.nav.photos"), subtitle)].spacing(16);
        let Some(phone) = phones.iter().find(|p| p.id == g.peer).or(phones.first()) else {
            return col
                .push(self.empty_phone(c, t("gui.photos.none_detail")))
                .into();
        };
        if g.peer != phone.id && !g.loading {
            col = col.push(
                button(text(tf("gui.photos.view_of", &[("name", &phone.name)])).font(SEMIBOLD))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::LoadPhotos {
                        peer: phone.id.clone(),
                        more: false,
                    }),
            );
            return col.into();
        }
        if let Some(err) = &g.error {
            col = col.push(text(err).color(c.error));
        }
        col = col.push(self.photos_toolbar(c));
        // Columnas según el ancho de la página (~190 px por foto).
        let columns = ((self.layout().page_width - 56.0) / 190.0)
            .floor()
            .clamp(2.0, 8.0) as usize;
        for chunk in g.photos.chunks(columns) {
            let mut r = row![].spacing(10);
            for (id, _name, _date, _w, _h, _size, thumb, _original) in chunk {
                // Entrada animada de las fotos nuevas: aparecen (opacidad) y
                // llevan un borde del color primario que se desvanece.
                let age = g.fresh.get(id).map(|at| at.elapsed());
                let opacity = age.map_or(1.0, |a| {
                    (a.as_secs_f32() / PHOTO_FADE.as_secs_f32()).clamp(0.0, 1.0)
                });
                let glow = age.map_or(0.0, |a| {
                    1.0 - (a.as_secs_f32() / PHOTO_HIGHLIGHT.as_secs_f32()).clamp(0.0, 1.0)
                });
                let tile = container(
                    iced::widget::image(iced::widget::image::Handle::from_path(thumb))
                        .width(Length::Fill)
                        .height(Length::Fixed(150.0))
                        .content_fit(iced::ContentFit::Cover)
                        .opacity(opacity),
                )
                .padding(if glow > 0.0 { 3 } else { 0 })
                .style(move |_: &Theme| container::Style {
                    border: iced::Border {
                        color: Color {
                            a: glow,
                            ..c.primary
                        },
                        width: if glow > 0.0 { 3.0 } else { 0.0 },
                        radius: 14.0.into(),
                    },
                    ..Default::default()
                });
                r = r.push(
                    button(tile)
                        .padding(0)
                        .width(Length::FillPortion(1))
                        .style(style::button(
                            c,
                            ButtonKind::Card {
                                bg: c.container_high,
                                fg: c.on_surface,
                                shape: Shape::List,
                            },
                        ))
                        .on_press(Message::FetchPhoto {
                            peer: g.peer.clone(),
                            id: id.clone(),
                        }),
                );
            }
            for _ in chunk.len()..columns {
                r = r.push(space().width(Length::FillPortion(1)));
            }
            col = col.push(r);
        }
        if g.photos.is_empty() && g.loading {
            col = col.push(text(t("gui.common.loading")).color(c.on_surface_variant));
        }
        // "Cargar más" también al final, para no tener que volver arriba.
        if !g.loading && (g.photos.len() as u32) < g.total {
            col = col.push(
                container(
                    button(labeled(
                        Icon::ExpandMore,
                        t("gui.photos.load_more"),
                        c.on_secondary_container,
                    ))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::LoadPhotos {
                        peer: g.peer.clone(),
                        more: true,
                    }),
                )
                .center_x(Length::Fill),
            );
        }
        col.into()
    }

    /// Barra de acciones de la página Fotos (arriba).
    pub(crate) fn photos_toolbar(&self, c: Colors) -> Element<'_, Message> {
        let g = &self.gallery;
        let status: Element<'_, Message> = if g.loading && !g.photos.is_empty() {
            row![
                Icon::Refresh.view(16.0, c.on_surface_variant),
                text(t("gui.photos.updating")).color(c.on_surface_variant)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else {
            space().into()
        };
        row![
            button(labeled(
                Icon::Refresh,
                t("gui.photos.reload"),
                c.on_secondary_container
            ))
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Tonal))
            .on_press_maybe((!g.loading).then(|| Message::LoadPhotos {
                peer: g.peer.clone(),
                more: false,
            })),
            button(labeled(
                Icon::ExpandMore,
                t("gui.photos.load_more"),
                c.primary
            ))
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Outlined))
            .on_press_maybe((!g.loading && (g.photos.len() as u32) < g.total).then(|| {
                Message::LoadPhotos {
                    peer: g.peer.clone(),
                    more: true,
                }
            })),
            button(labeled(
                Icon::Folder,
                t("gui.common.open_downloads"),
                c.primary
            ))
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Outlined))
            .on_press(Message::OpenDownloads),
            space().width(Length::Fill),
            status,
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    }
}

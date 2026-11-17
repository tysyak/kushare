//! SMS, llamadas, contactos, notificaciones activas y fondo de pantalla del
//! teléfono.

use super::*;

impl Features {
    // -----------------------------------------------------------------
    // SMS, llamadas, contactos, notificaciones y fondo de pantalla
    // -----------------------------------------------------------------

    pub async fn sms_threads(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<pb::SmsThreads> {
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::SmsThreadsRequest(pb::SmsThreadsRequest {
                        request_id,
                        offset,
                        limit: limit.clamp(1, 100),
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::SmsThreads(t) if t.error.is_empty() => Ok(t),
            F::SmsThreads(t) => anyhow::bail!("{}", phone_error(&t.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    pub async fn sms_messages(
        &self,
        peer_id: &str,
        thread_id: &str,
        limit: u32,
        before_ms: i64,
    ) -> anyhow::Result<Vec<pb::SmsMessage>> {
        self.bulk_for_request(peer_id).await;
        let thread_id = thread_id.to_string();
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::SmsMessagesRequest(pb::SmsMessagesRequest {
                        request_id,
                        thread_id,
                        limit: limit.clamp(1, 200),
                        before_ms,
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::SmsMessages(m) if m.error.is_empty() => Ok(m.messages),
            F::SmsMessages(m) => anyhow::bail!("{}", phone_error(&m.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    pub async fn send_sms(&self, peer_id: &str, address: &str, body: &str) -> anyhow::Result<()> {
        if address.trim().is_empty() || body.trim().is_empty() {
            anyhow::bail!("falta el número o el texto");
        }
        let (address, body) = (address.trim().to_string(), body.to_string());
        self.command(peer_id, |request_id| {
            F::SendSms(pb::SendSms {
                request_id,
                address,
                body,
            })
        })
        .await
        .map(|_| ())
    }

    pub async fn call_log(
        &self,
        peer_id: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<pb::CallLogEntry>> {
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::CallLogRequest(pb::CallLogRequest {
                        request_id,
                        limit: limit.clamp(1, 500),
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::CallLog(l) if l.error.is_empty() => Ok(l.entries),
            F::CallLog(l) => anyhow::bail!("{}", phone_error(&l.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    /// Contactos del teléfono (con número). Se reutilizan unos minutos.
    pub async fn contacts(&self, peer_id: &str, refresh: bool) -> anyhow::Result<Vec<pb::Contact>> {
        if !refresh
            && let Some((at, list)) = self.contacts.lock().expect("mutex envenenado").get(peer_id)
            && at.elapsed() < CONTACTS_TTL
        {
            return Ok(list.clone());
        }
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| F::ContactsRequest(pb::ContactsRequest { request_id }),
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        let list = match reply {
            F::Contacts(c) if c.error.is_empty() => c.contacts,
            F::Contacts(c) => anyhow::bail!("{}", phone_error(&c.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        };
        self.contacts
            .lock()
            .expect("mutex envenenado")
            .insert(peer_id.to_string(), (Instant::now(), list.clone()));
        Ok(list)
    }

    /// Ruta de una foto de contacto. La extensión importa: el visor de
    /// imágenes de la GUI deduce el formato por ella.
    pub(super) fn contact_photo_path(&self, peer_id: &str, key: &str, ext: &str) -> PathBuf {
        self.cache_dir
            .join("contacts")
            .join(sanitize(peer_id))
            .join(format!("{}.{ext}", sanitize(key)))
    }

    /// Foto de contacto ya guardada en este PC (sin pedir nada al teléfono).
    pub fn cached_contact_photo(&self, peer_id: &str, key: &str) -> Option<PathBuf> {
        if key.is_empty() {
            return None;
        }
        ["jpg", "png"]
            .into_iter()
            .map(|ext| self.contact_photo_path(peer_id, key, ext))
            .find(|path| path.exists())
    }

    /// Fotos de contacto por clave: las de la caché y, en una sola petición
    /// corta, las que faltan. Si el teléfono no responde (o es una versión
    /// sin fotos), simplemente no hay foto.
    pub async fn contact_photos(
        &self,
        peer_id: &str,
        keys: impl IntoIterator<Item = String>,
    ) -> HashMap<String, PathBuf> {
        let mut found = HashMap::new();
        let mut missing: Vec<String> = Vec::new();
        {
            let skip = self.photo_missing.lock().expect("mutex envenenado");
            for key in keys {
                if key.is_empty() || found.contains_key(&key) || missing.contains(&key) {
                    continue;
                }
                match self.cached_contact_photo(peer_id, &key) {
                    Some(path) => {
                        found.insert(key, path);
                    }
                    None if !skip.contains(&(peer_id.to_string(), key.clone())) => {
                        missing.push(key)
                    }
                    None => {}
                }
            }
        }
        missing.truncate(CONTACT_PHOTOS_MAX);
        if missing.is_empty() {
            return found;
        }
        self.bulk_for_request(peer_id).await;
        let asked = missing.clone();
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::ContactPhotosRequest(pb::ContactPhotosRequest {
                        request_id,
                        keys: asked,
                    })
                },
                CONTACT_PHOTOS_TIMEOUT,
            )
            .await;
        let photos = match reply {
            Ok(F::ContactPhotos(p)) if p.error.is_empty() => p.photos,
            Ok(F::ContactPhotos(p)) => {
                tracing::debug!("fotos de contacto: {}", p.error);
                return found;
            }
            _ => return found,
        };
        for photo in photos {
            if !missing.contains(&photo.key) || photo.image.is_empty() {
                continue;
            }
            let ext = image_extension(&photo.image);
            let path = self.contact_photo_path(peer_id, &photo.key, ext);
            match write_private(&path, &photo.image) {
                Ok(()) => {
                    found.insert(photo.key, path);
                }
                Err(err) => tracing::warn!("no se pudo guardar una foto de contacto: {err}"),
            }
        }
        let mut skip = self.photo_missing.lock().expect("mutex envenenado");
        for key in missing {
            if !found.contains_key(&key) {
                skip.insert((peer_id.to_string(), key));
            }
        }
        found
    }

    /// Llama a `number` desde el teléfono.
    pub async fn dial(&self, peer_id: &str, number: &str) -> anyhow::Result<()> {
        let number = dialable(number);
        if number.is_empty() {
            anyhow::bail!("número vacío");
        }
        self.command(peer_id, |request_id| {
            F::Dial(pb::Dial { request_id, number })
        })
        .await
        .map(|_| ())
    }

    /// Notificaciones que hay ahora en el teléfono, con la ruta de su icono.
    pub async fn phone_notifications(
        &self,
        peer_id: &str,
    ) -> anyhow::Result<Vec<(pb::Notification, Option<PathBuf>)>> {
        self.bulk_for_request(peer_id).await;
        let have_icons = self.manager.cached_icon_hashes();
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::ActiveNotificationsRequest(pb::ActiveNotificationsRequest {
                        request_id,
                        have_icons,
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        let list = match reply {
            F::ActiveNotifications(a) if a.error.is_empty() => a.notifications,
            F::ActiveNotifications(a) => anyhow::bail!("{}", phone_error(&a.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        };
        Ok(list
            .into_iter()
            .map(|mut n| {
                let icon = (!n.icon_hash.is_empty())
                    .then(|| self.manager.cached_icon(&n.icon_hash, &n.icon_png))
                    .flatten();
                n.icon_png.clear();
                (n, icon)
            })
            .collect())
    }

    pub(super) fn wallpaper_path(&self, peer_id: &str) -> PathBuf {
        self.cache_dir
            .join("wallpapers")
            .join(format!("{}.jpg", sanitize(peer_id)))
    }

    /// Fondo de pantalla del teléfono. Sin `refresh`, el de la caché (el
    /// del disco sirve incluso sin conexión).
    pub async fn wallpaper(&self, peer_id: &str, refresh: bool) -> anyhow::Result<WallpaperInfo> {
        if !refresh {
            if let Some(w) = self
                .wallpapers
                .lock()
                .expect("mutex envenenado")
                .get(peer_id)
            {
                return Ok(w.clone());
            }
            let path = self.wallpaper_path(peer_id);
            if path.exists() && !self.manager.is_connected(peer_id) {
                return Ok(WallpaperInfo {
                    path: Some(path),
                    colors: Vec::new(),
                });
            }
        }
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::WallpaperRequest(pb::WallpaperRequest {
                        request_id,
                        max_size: 1280,
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        let F::Wallpaper(w) = reply else {
            anyhow::bail!("respuesta inesperada del teléfono");
        };
        let path = self.wallpaper_path(peer_id);
        let path = if w.jpeg.is_empty() {
            let _ = std::fs::remove_file(&path);
            None
        } else {
            write_private(&path, &w.jpeg)?;
            Some(path)
        };
        let info = WallpaperInfo {
            path,
            colors: w.colors,
        };
        self.wallpapers
            .lock()
            .expect("mutex envenenado")
            .insert(peer_id.to_string(), info.clone());
        Ok(info)
    }
}

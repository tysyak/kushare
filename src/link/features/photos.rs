//! Fotos del teléfono: galería, miniaturas, descargas, precarga, avisos de
//! fotos nuevas y límites de la caché.

use super::*;

impl Features {
    // -----------------------------------------------------------------
    // Fotos
    // -----------------------------------------------------------------

    pub(super) fn thumb_dir(&self, peer_id: &str) -> PathBuf {
        self.cache_dir.join("thumbs").join(sanitize(peer_id))
    }

    pub(super) fn photo_dir(&self, peer_id: &str) -> PathBuf {
        self.cache_dir.join("photos").join(sanitize(peer_id))
    }

    /// La original de `id` en la caché (`<id>__<nombre>`), si está.
    pub fn original_path(&self, peer_id: &str, id: &str) -> Option<PathBuf> {
        let prefix = format!("{}__", sanitize(id));
        std::fs::read_dir(self.photo_dir(peer_id))
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
    }

    pub(super) fn entry(&self, peer_id: &str, info: pb::PhotoInfo) -> PhotoEntry {
        PhotoEntry {
            thumb: self
                .thumb_dir(peer_id)
                .join(format!("{}.jpg", sanitize(&info.id))),
            original: self.original_path(peer_id, &info.id),
            info,
        }
    }

    /// Página de la galería del peer (más reciente primero); guarda las
    /// miniaturas en la caché. Con `offset == 0` además recuerda la lista
    /// (para [`Self::cached_photos`]) y precarga las más recientes.
    pub async fn photos(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<(u32, Vec<PhotoEntry>)> {
        let (total, photos) = self.list_photos(peer_id, offset, limit).await?;
        if offset == 0 {
            self.save_index(peer_id, total, &photos);
            let _ = self.prefetch.send(peer_id.to_string());
        }
        Ok((total, photos))
    }

    pub(super) async fn list_photos(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<(u32, Vec<PhotoEntry>)> {
        self.bulk_for_request(peer_id).await;
        let dir = self.thumb_dir(peer_id);
        create_private_dir(&dir)?;
        let have = cached_thumbnails(&dir);
        let mut out = Vec::new();
        let mut total = 0;
        let limit = limit.max(1);
        while (out.len() as u32) < limit {
            let page_offset = offset + out.len() as u32;
            let page = (limit - out.len() as u32).min(PHOTO_PAGE_MAX);
            let have_thumbnails = have.clone();
            let reply = self
                .request(
                    peer_id,
                    |request_id| {
                        F::PhotoListRequest(pb::PhotoListRequest {
                            request_id,
                            offset: page_offset,
                            limit: page,
                            have_thumbnails,
                        })
                    },
                    SLOW_REQUEST_TIMEOUT,
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("el teléfono no respondió (¿falta el permiso de fotos?)")
                })?;
            let F::PhotoList(list) = reply else {
                anyhow::bail!("respuesta inesperada del teléfono");
            };
            total = list.total;
            let got = list.photos.len();
            for mut photo in list.photos {
                if !photo.thumbnail.is_empty() {
                    let path = dir.join(format!("{}.jpg", sanitize(&photo.id)));
                    write_private(&path, &photo.thumbnail)?;
                }
                photo.thumbnail.clear();
                out.push(self.entry(peer_id, photo));
            }
            if got < page as usize {
                break;
            }
        }
        Ok((total, out))
    }

    pub(super) fn index_path(&self, peer_id: &str) -> PathBuf {
        self.thumb_dir(peer_id).join("index.json")
    }

    pub(super) fn save_index(&self, peer_id: &str, total: u32, photos: &[PhotoEntry]) {
        let index = PhotoIndex {
            total,
            photos: photos
                .iter()
                .map(|p| IndexedPhoto {
                    id: p.info.id.clone(),
                    name: p.info.name.clone(),
                    date_ms: p.info.date_ms,
                    width: p.info.width,
                    height: p.info.height,
                    size: p.info.size,
                })
                .collect(),
        };
        if let Ok(json) = serde_json::to_vec(&index)
            && let Err(err) = write_private(&self.index_path(peer_id), &json)
        {
            tracing::debug!(?err, "no se pudo guardar el índice de fotos");
        }
    }

    /// La última lista de fotos vista (sin preguntar al teléfono): la
    /// galería se enseña al instante y también sin conexión.
    pub fn cached_photos(&self, peer_id: &str) -> (u32, Vec<PhotoEntry>) {
        let index: PhotoIndex = std::fs::read(self.index_path(peer_id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let photos = index
            .photos
            .into_iter()
            .map(|p| {
                self.entry(
                    peer_id,
                    pb::PhotoInfo {
                        id: p.id,
                        name: p.name,
                        date_ms: p.date_ms,
                        width: p.width,
                        height: p.height,
                        size: p.size,
                        thumbnail: Vec::new(),
                    },
                )
            })
            .filter(|e| e.thumb.exists())
            .collect();
        (index.total, photos)
    }

    /// La foto original `id` en la caché (la descarga si no está).
    pub async fn fetch_photo(&self, peer_id: &str, id: &str) -> anyhow::Result<PathBuf> {
        self.download(peer_id, id, false).await
    }

    /// Descarga la original a la carpeta de descargas (`kushare photos --download`).
    pub async fn save_photo_to_downloads(
        &self,
        peer_id: &str,
        id: &str,
    ) -> anyhow::Result<PathBuf> {
        let cached = self.fetch_photo(peer_id, id).await?;
        let dir = directories::UserDirs::new()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir)?;
        let target = unique_path(&dir, &original_name(&cached));
        std::fs::copy(&cached, &target)?;
        Ok(target)
    }

    pub(super) async fn download(
        &self,
        peer_id: &str,
        id: &str,
        background: bool,
    ) -> anyhow::Result<PathBuf> {
        let key = (peer_id.to_string(), id.to_string());
        // Si ya se está descargando (p. ej. la precarga), esperar a esa.
        let started = Instant::now();
        while self
            .inflight
            .lock()
            .expect("mutex envenenado")
            .contains(&key)
        {
            if started.elapsed() > BACKGROUND_DOWNLOAD_TIMEOUT {
                anyhow::bail!("la descarga de la foto no terminó a tiempo");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if let Some(path) = self.original_path(peer_id, id) {
            return Ok(path);
        }
        self.inflight
            .lock()
            .expect("mutex envenenado")
            .insert(key.clone());
        let result = self.download_now(peer_id, id, background).await;
        self.inflight.lock().expect("mutex envenenado").remove(&key);
        result
    }

    pub(super) async fn download_now(
        &self,
        peer_id: &str,
        id: &str,
        background: bool,
    ) -> anyhow::Result<PathBuf> {
        self.bulk_for_request(peer_id).await;
        let request_id = self.new_request_id();
        let (tx, rx) = oneshot::channel();
        let dir = self.photo_dir(peer_id);
        create_private_dir(&dir)?;
        let part = dir.join(format!(".kushare-{request_id}.part"));
        let file = std::fs::File::create(&part)?;
        self.downloads.lock().expect("mutex envenenado").insert(
            request_id,
            Download {
                file,
                part: part.clone(),
                // El nombre definitivo se completa con el que mande el teléfono.
                target: dir.join(format!("{}__", sanitize(id))),
                done: tx,
            },
        );
        if let Err(err) = self.send(
            peer_id,
            F::PhotoRequest(pb::PhotoRequest {
                request_id,
                id: id.to_string(),
                chunk_size: if background { PREFETCH_CHUNK } else { 0 },
                background,
            }),
        ) {
            self.abort_download(request_id);
            return Err(err);
        }
        let timeout = if background {
            BACKGROUND_DOWNLOAD_TIMEOUT
        } else {
            DOWNLOAD_TIMEOUT
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            _ => {
                self.abort_download(request_id);
                anyhow::bail!("la descarga de la foto no terminó a tiempo")
            }
        }
    }

    pub(super) fn abort_download(&self, request_id: u64) {
        if let Some(d) = self
            .downloads
            .lock()
            .expect("mutex envenenado")
            .remove(&request_id)
        {
            let _ = std::fs::remove_file(&d.part);
        }
    }

    pub(super) fn on_photo_data(&self, data: pb::PhotoData) {
        use std::io::{Seek, SeekFrom, Write};
        let mut downloads = self.downloads.lock().expect("mutex envenenado");
        let Some(d) = downloads.get_mut(&data.request_id) else {
            return;
        };
        let written = d
            .file
            .seek(SeekFrom::Start(data.offset))
            .and_then(|_| d.file.write_all(&data.data));
        let failure = if !data.error.is_empty() {
            Some(anyhow::anyhow!(
                "el teléfono no pudo leer la foto: {}",
                data.error
            ))
        } else {
            written.err().map(anyhow::Error::from)
        };
        if failure.is_none() && !data.last {
            return;
        }
        let d = downloads.remove(&data.request_id).expect("existe");
        drop(downloads);
        let result = match failure {
            Some(err) => {
                let _ = std::fs::remove_file(&d.part);
                Err(err)
            }
            None => {
                let mut target = d.target.into_os_string();
                target.push(sanitize_file_name(&data.name, &data.id));
                let target = PathBuf::from(target);
                std::fs::rename(&d.part, &target)
                    .map(|_| target)
                    .map_err(Into::into)
            }
        };
        let _ = d.done.send(result);
    }

    /// Borra la foto del teléfono. "done", o "confirm" si el teléfono pide
    /// confirmarlo allí (Android protege las fotos de otras apps).
    pub async fn delete_photo(&self, peer_id: &str, id: &str) -> anyhow::Result<String> {
        let photo = id.to_string();
        let status = self
            .command(peer_id, |request_id| {
                F::PhotoDelete(pb::PhotoDelete {
                    request_id,
                    id: photo,
                })
            })
            .await?;
        if status == "done" {
            if let Some(path) = self.original_path(peer_id, id) {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_file(
                self.thumb_dir(peer_id)
                    .join(format!("{}.jpg", sanitize(id))),
            );
        }
        Ok(status)
    }

    /// Avisa (una vez) de las fotos nuevas `new` de `peer`.
    pub(super) fn announce(&self, peer: &str, new: &[PhotoEntry]) {
        let Some(first) = new.first() else {
            return;
        };
        if !self.settings.get().photo_notify {
            return;
        }
        // No repetir el aviso del mismo lote.
        static LAST: Mutex<Option<String>> = Mutex::new(None);
        let key = format!("{peer}/{}", first.info.id);
        {
            let mut last = LAST.lock().expect("mutex envenenado");
            if last.as_deref() == Some(key.as_str()) {
                return;
            }
            *last = Some(key);
        }
        let photos = new
            .iter()
            .map(|p| PhotoEntry {
                original: self.original_path(peer, &p.info.id),
                ..p.clone()
            })
            .collect();
        let _ = self.new_photos.send(NewPhotos {
            peer: peer.to_string(),
            photos,
        });
    }

    /// Deja cada carpeta de la caché por debajo de su límite (borra primero
    /// lo que hace más que no se usa). Los iconos y miniaturas que se borren
    /// se vuelven a pedir solos: lo que hay se calcula leyendo la carpeta.
    pub(super) fn prune_photo_cache(&self) {
        for (dir, max) in CACHE_LIMITS {
            prune_dir(&self.cache_dir.join(dir), *max);
        }
    }
}

/// Límites de la caché (`~/.cache/kushare/<carpeta>`), en bytes.
pub(super) const CACHE_LIMITS: &[(&str, u64)] = &[
    ("photos", PHOTO_CACHE_MAX_BYTES),
    ("thumbs", 64 * 1024 * 1024),
    ("icons", 32 * 1024 * 1024),
    ("art", 32 * 1024 * 1024),
    ("contacts", 32 * 1024 * 1024),
];

/// Borra los archivos más antiguos (por último uso) de `root` y de sus
/// subcarpetas (una por teléfono) hasta que ocupen como mucho `max` bytes.
pub(super) fn prune_dir(root: &Path, max: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let collect = |dir: &Path, files: &mut Vec<_>| {
        for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if let Ok(meta) = f.metadata()
                && meta.is_file()
            {
                let used = meta.accessed().or_else(|_| meta.modified());
                files.push((used.unwrap_or(std::time::UNIX_EPOCH), meta.len(), f.path()));
            }
        }
    };
    collect(root, &mut files);
    for sub in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if sub.file_type().is_ok_and(|t| t.is_dir()) {
            collect(&sub.path(), &mut files);
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= max {
        return;
    }
    files.sort();
    for (_, len, path) in files {
        if total <= max {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
    tracing::info!(dir = %root.display(), mb = total / 1024 / 1024, "caché recortada");
}

/// Precarga en segundo plano las fotos más recientes de cada peer que lo
/// pida (al conectar, al haber fotos nuevas, al abrir la galería).
pub(super) async fn prefetch_task(
    features: Arc<Features>,
    mut rx: mpsc::UnboundedReceiver<String>,
) {
    while let Some(first) = rx.recv().await {
        // Agrupar avisos seguidos (una ráfaga de capturas, etc.).
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut peers = HashSet::from([first]);
        while let Ok(p) = rx.try_recv() {
            peers.insert(p);
        }
        for peer in peers {
            let settings = features.settings.get();
            if !(settings.photo_prefetch || settings.photo_notify)
                || !features.manager.is_connected(&peer)
            {
                continue;
            }
            let list = match features.list_photos(&peer, 0, PREFETCH_COUNT).await {
                Ok((_, list)) => list,
                Err(err) => {
                    tracing::debug!(?err, "precarga de fotos: no se pudo listar");
                    continue;
                }
            };
            // Nuevas: posteriores a la más reciente ya vista. La primera vez
            // (al arrancar) solo se toma nota, sin avisar.
            let newest = list.iter().map(|p| p.info.date_ms).max().unwrap_or(0);
            let seen = features
                .newest_seen
                .lock()
                .expect("mutex envenenado")
                .insert(peer.clone(), newest);
            let new: Vec<PhotoEntry> = match seen {
                Some(seen) => list
                    .iter()
                    .filter(|p| p.info.date_ms > seen)
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            // Solo por Wi-Fi (`list_photos` ya probó el enlace de datos): por
            // Bluetooth, precargar fotos ocupa el enlace minutos y retrasa todo
            // lo demás (SMS, llamadas, notificaciones). Las que se abren a mano
            // se siguen descargando por Bluetooth.
            let wifi = features.lan_ready(&peer) || features.manager.has_bulk(&peer);
            if settings.photo_prefetch && wifi {
                // Las nuevas primero, para que "Ver" del aviso sea instantáneo.
                for photo in new.iter().chain(list.iter()) {
                    if features.original_path(&peer, &photo.info.id).is_some()
                        || photo.info.size > PREFETCH_MAX_BYTES
                    {
                        continue;
                    }
                    if !features.manager.is_connected(&peer) {
                        break;
                    }
                    match features.download(&peer, &photo.info.id, true).await {
                        Ok(path) => tracing::debug!(?path, "foto precargada"),
                        Err(err) => tracing::debug!(?err, "precarga de foto fallida"),
                    }
                    // Avisar en cuanto esté la primera nueva (no esperar a las demás).
                    if !new.is_empty() && photo.info.id == new[0].info.id {
                        features.announce(&peer, &new);
                    }
                }
                features.prune_photo_cache();
            }
            features.announce(&peer, &new);
        }
    }
}

/// Ids de las miniaturas en caché (las más recientes primero, hasta 1000).
pub(super) fn cached_thumbnails(dir: &Path) -> Vec<String> {
    let mut thumbs: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".jpg")?.to_string();
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, id))
        })
        .collect();
    thumbs.sort_by_key(|t| std::cmp::Reverse(t.0));
    thumbs.into_iter().take(1000).map(|(_, id)| id).collect()
}

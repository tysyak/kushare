//! «Compartir internet»: punto de acceso Wi-Fi del PC para el teléfono.
//!
//! - **Puente con DHCP y NAT**: conexión volátil de NetworkManager
//!   (`kushare-hotspot`, `ipv4.method shared`) sobre [`BRIDGE`]; la crea el
//!   propio usuario con `nmcli`.
//! - **El AP**: lo monta el ayudante root `kushare-hotspot` (ver
//!   src/bin/kushare-hotspot), lanzado con `pkexec` (polkit pide la
//!   contraseña una vez y la recuerda un rato). Recibe la red por stdin y
//!   avisa de su estado por stdout; al cerrarle stdin lo desmonta todo, así
//!   que si el daemon muere no quedan restos.
//! - **Configuración**: `~/.config/kushare/hotspot.toml` (0600: lleva la
//!   clave), con nombre `kushare-<equipo>` y clave aleatoria por defecto.
//! - **Antena**: el AP tiene que ir en el canal de la Wi-Fi del PC, y en
//!   algunos la tarjeta no puede emitir («no IR»). Si la red del PC tiene
//!   otra antena en un canal válido, se ofrece pasarse a ella: se fija su
//!   BSSID en el perfil de NetworkManager mientras la red esté encendida
//!   (lo que había queda en `hotspot-antena.toml` y se restaura al apagar, o
//!   al arrancar si el daemon se cerró de golpe).

use super::hotspot_shared::{self as shared, BRIDGE, HELPER_PATH, HelperConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::watch;

/// Nombre de la conexión de NetworkManager del puente.
const NM_CONNECTION: &str = "kushare-hotspot";
/// Red del puente (el PC es la .1; el DHCP de NM reparte el resto).
const BRIDGE_ADDRESS: &str = "10.42.100.1/24";
/// Cuánto se espera a que el AP esté listo (incluye el diálogo de polkit).
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// Señal mínima (0–100, la de `nmcli`) para proponer otra antena.
const MIN_SIGNAL: u32 = 25;

/// Antena de la red del PC fijada mientras la red compartida esté encendida.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pinned {
    /// UUID de la conexión de NetworkManager.
    connection: String,
    /// BSSID que tenía fijado el perfil antes (normalmente ninguno).
    previous_bssid: String,
}

/// Otra antena (punto de acceso) de la misma red.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Antenna {
    bssid: String,
    channel: u32,
    signal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Config {
    ssid: String,
    password: String,
}

/// Estado para la GUI, la CLI y la TUI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub active: bool,
    /// Arrancando (esperando a polkit o a hostapd).
    pub starting: bool,
    pub ssid: String,
    pub password: String,
    /// «5 GHz · canal 149» cuando está activo.
    pub band: String,
    pub clients: u32,
    /// Último error (vacío si no hay).
    pub error: String,
    /// Si el canal de la Wi-Fi del PC no sirve y hay otra antena de la misma
    /// red que sí: «canal 157 · señal 45» (vacío si no).
    pub alternative: String,
    /// La Wi-Fi del PC está fijada en otra antena mientras la red esté
    /// encendida: «canal 1 · señal 57» (vacío si no).
    pub switched: String,
}

/// El canal no sirve pero hay otra antena: [`Hotspot::start`] se pasa a ella
/// sola si `auto_antenna` está activo.
#[derive(Debug)]
struct NeedsAntenna;

impl std::fmt::Display for NeedsAntenna {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("hace falta cambiar de antena")
    }
}

impl std::error::Error for NeedsAntenna {}

struct Session {
    child: Child,
    /// Mientras siga abierto, el ayudante mantiene el AP.
    stdin: Option<ChildStdin>,
}

pub struct Hotspot {
    path: PathBuf,
    pin_path: PathBuf,
    default_ssid: String,
    status: Mutex<Status>,
    session: tokio::sync::Mutex<Option<Session>>,
    changed: watch::Sender<u64>,
    /// Puerto real del enlace (se abre en el cortafuegos del puente).
    link_port: std::sync::atomic::AtomicU16,
    /// Ajuste `hotspot-auto-antenna` (lo fija quien la enciende).
    auto_antenna: std::sync::atomic::AtomicBool,
}

impl Hotspot {
    /// Carga (o crea) la configuración y quita los restos de un daemon
    /// anterior que no se cerró bien.
    pub fn open(config_dir: &Path, device_name: &str) -> Arc<Self> {
        let path = config_dir.join("hotspot.toml");
        let pin_path = config_dir.join("hotspot-antena.toml");
        let default_ssid = default_ssid(device_name);
        let config = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str::<Config>(&s).ok())
            .filter(|c| shared::validate_ssid(&c.ssid).is_ok())
            .filter(|c| shared::validate_password(&c.password).is_ok());
        let config = config.unwrap_or_else(|| Config {
            ssid: default_ssid.clone(),
            password: shared::random_password(12),
        });
        let this = Arc::new(Self {
            path,
            pin_path,
            default_ssid,
            status: Mutex::new(Status {
                ssid: config.ssid.clone(),
                password: config.password.clone(),
                ..Status::default()
            }),
            session: tokio::sync::Mutex::new(None),
            changed: watch::channel(0).0,
            link_port: std::sync::atomic::AtomicU16::new(0),
            auto_antenna: std::sync::atomic::AtomicBool::new(true),
        });
        if let Err(err) = this.save(&config) {
            tracing::warn!(?err, "no se pudo guardar hotspot.toml");
        }
        let restore = Arc::clone(&this);
        tokio::spawn(async move {
            let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
            restore.unpin().await;
        });
        this
    }

    /// El puerto TCP en el que escucha el enlace (se conoce después de abrir
    /// el `Hotspot`).
    pub fn set_link_port(&self, port: u16) {
        self.link_port
            .store(port, std::sync::atomic::Ordering::Relaxed);
    }

    /// Si al encender se puede cambiar sola de antena (ajuste
    /// `hotspot-auto-antenna`).
    pub fn set_auto_antenna(&self, on: bool) {
        self.auto_antenna
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn status(&self) -> Status {
        self.status.lock().expect("mutex envenenado").clone()
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        let changed = {
            let mut st = self.status.lock().expect("mutex envenenado");
            let before = st.clone();
            f(&mut st);
            *st != before
        };
        if changed {
            self.changed.send_modify(|n| *n = n.wrapping_add(1));
        }
    }

    fn save(&self, config: &Config) -> anyhow::Result<()> {
        let text = toml::to_string(config)?;
        super::features::write_private(&self.path, text.as_bytes())?;
        Ok(())
    }

    /// Cambia el nombre y la clave (se aplican la próxima vez que se
    /// encienda). Un nombre vacío vuelve al de por defecto.
    pub fn set_config(&self, ssid: &str, password: &str) -> anyhow::Result<()> {
        let ssid = if ssid.trim().is_empty() {
            self.default_ssid.clone()
        } else {
            ssid.trim().to_string()
        };
        shared::validate_ssid(&ssid).map_err(anyhow::Error::msg)?;
        shared::validate_password(password).map_err(anyhow::Error::msg)?;
        let config = Config {
            ssid: ssid.clone(),
            password: password.to_string(),
        };
        self.save(&config)?;
        self.update(|st| {
            st.ssid = ssid;
            st.password = password.to_string();
        });
        Ok(())
    }

    /// Texto del QR para conectarse.
    pub fn qr(&self) -> String {
        let st = self.status();
        shared::wifi_qr(&st.ssid, &st.password)
    }

    /// Marca «arrancando» ya, antes de que la tarea de [`Self::start`] llegue
    /// a ejecutarse (para que quien consulte el estado justo después no vea
    /// la red apagada y sin error).
    pub fn mark_starting(&self) {
        self.update(|st| {
            if !st.active {
                st.starting = true;
                st.error.clear();
            }
        });
    }

    /// Enciende el AP. Vuelve cuando está listo o cuando falla.
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut session = self.session.lock().await;
        if session.is_some() {
            return Ok(());
        }
        self.update(|st| {
            st.error.clear();
            st.starting = true;
        });
        match self.spawn().await {
            Ok((child, stdin, ready)) => {
                *session = Some(Session {
                    child,
                    stdin: Some(stdin),
                });
                drop(session);
                self.wait_ready(ready).await
            }
            Err(err) if err.is::<NeedsAntenna>() => {
                drop(session);
                let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
                tracing::info!("el canal de la Wi-Fi no sirve: se cambia de antena sola");
                Box::pin(self.switch_antenna_and_start()).await
            }
            Err(err) => {
                drop(session);
                let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
                self.unpin().await;
                self.update(|st| {
                    st.starting = false;
                    st.error = err.to_string();
                });
                Err(err)
            }
        }
    }

    /// Pasa la Wi-Fi del PC a la antena compatible que se ofreció (ver
    /// [`Status::alternative`]) y enciende la red.
    pub async fn switch_antenna_and_start(self: &Arc<Self>) -> anyhow::Result<()> {
        let result = async {
            let base = base_wifi().await?;
            let plan = find_alternative(&base.name, base.channel)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no hay otra antena de esta red en un canal válido"))?;
            let uuid = nmcli(&["-g", "GENERAL.CON-UUID", "device", "show", &base.name])
                .await?
                .trim()
                .to_string();
            anyhow::ensure!(!uuid.is_empty(), "la Wi-Fi del PC no está conectada");
            let previous = unescape_terse(
                nmcli(&["-g", "802-11-wireless.bssid", "connection", "show", &uuid])
                    .await?
                    .trim(),
            );
            let pinned = Pinned {
                connection: uuid.clone(),
                previous_bssid: previous,
            };
            super::features::write_private(&self.pin_path, toml::to_string(&pinned)?.as_bytes())?;
            tracing::info!(bssid = %plan.bssid, channel = plan.channel, "fijando otra antena para compartir internet");
            let label = format!("canal {} · señal {}", plan.channel, plan.signal);
            self.update(|st| st.switched = label);
            nmcli(&["connection", "modify", &uuid, "802-11-wireless.bssid", &plan.bssid]).await?;
            nmcli(&["connection", "up", &uuid]).await?;
            // Esperar a estar de verdad en el canal nuevo.
            for _ in 0..40 {
                if base_wifi().await.is_ok_and(|b| b.channel == Some(plan.channel)) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            anyhow::bail!("la Wi-Fi del PC no se pasó al canal {}", plan.channel)
        }
        .await;
        if let Err(err) = result {
            self.unpin().await;
            self.update(|st| {
                st.starting = false;
                st.error = err.to_string();
            });
            return Err(err);
        }
        self.update(|st| st.alternative.clear());
        self.start().await
    }

    /// Deja el perfil de la Wi-Fi del PC como estaba (si se fijó una antena).
    async fn unpin(&self) {
        let Some(pinned) = std::fs::read_to_string(&self.pin_path)
            .ok()
            .and_then(|s| toml::from_str::<Pinned>(&s).ok())
        else {
            return;
        };
        tracing::info!("restaurando la antena de la Wi-Fi del PC");
        let restored = nmcli(&[
            "connection",
            "modify",
            &pinned.connection,
            "802-11-wireless.bssid",
            &pinned.previous_bssid,
        ])
        .await;
        if let Err(err) = &restored {
            tracing::warn!(%err, "no se pudo restaurar la antena");
        }
        let _ = nmcli(&["connection", "up", &pinned.connection]).await;
        let _ = std::fs::remove_file(&self.pin_path);
        self.update(|st| st.switched.clear());
    }

    async fn wait_ready(
        self: &Arc<Self>,
        mut ready: watch::Receiver<Option<Result<(), String>>>,
    ) -> anyhow::Result<()> {
        let result = tokio::time::timeout(START_TIMEOUT, async {
            loop {
                if let Some(r) = ready.borrow_and_update().clone() {
                    return r;
                }
                if ready.changed().await.is_err() {
                    return Err("el ayudante terminó sin avisar".to_string());
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err("el punto de acceso no arrancó a tiempo".to_string()));
        if let Err(err) = &result {
            self.stop().await;
            self.update(|st| st.error = err.clone());
        }
        result.map_err(anyhow::Error::msg)
    }

    #[allow(clippy::type_complexity)]
    async fn spawn(
        self: &Arc<Self>,
    ) -> anyhow::Result<(
        Child,
        ChildStdin,
        watch::Receiver<Option<Result<(), String>>>,
    )> {
        if !Path::new(HELPER_PATH).is_file() {
            anyhow::bail!("falta el ayudante {HELPER_PATH} (se instala con el paquete de kushare)");
        }
        let wifi = base_wifi().await?;
        // ¿Puede la tarjeta emitir en el canal en el que está? Si no, ver si
        // hay otra antena de la misma red a la que pasarse.
        if let Some(channel) = wifi.channel
            && iw(&["phy"])
                .await
                .is_ok_and(|p| shared::channel_no_ir(&p, channel))
        {
            let alt = find_alternative(&wifi.name, wifi.channel)
                .await
                .ok()
                .flatten()
                .map(|a| format!("canal {} · señal {}", a.channel, a.signal))
                .unwrap_or_default();
            let has_alt = !alt.is_empty();
            self.update(|st| st.alternative = alt);
            if has_alt && self.auto_antenna.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(NeedsAntenna.into());
            }
            if has_alt {
                // El botón «Usar la antena…» ofrece la solución.
                anyhow::bail!(
                    "la Wi-Fi del PC está en el canal {channel}, donde esta tarjeta no puede emitir"
                );
            }
            anyhow::bail!(shared::no_ir_error(channel));
        }
        self.update(|st| st.alternative.clear());
        let base = wifi.name;

        // Puente con DHCP + NAT (volátil: desaparece al reiniciar NM).
        let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
        nmcli(&[
            "connection",
            "add",
            "save",
            "no",
            "type",
            "bridge",
            "ifname",
            BRIDGE,
            "con-name",
            NM_CONNECTION,
            "autoconnect",
            "no",
            "bridge.stp",
            "no",
            "ipv4.method",
            "shared",
            "ipv4.addresses",
            BRIDGE_ADDRESS,
            "ipv6.method",
            "ignore",
        ])
        .await?;
        nmcli(&["connection", "up", NM_CONNECTION]).await?;

        let st = self.status();
        let config = HelperConfig {
            base_iface: base,
            ssid: st.ssid,
            password: st.password,
            link_port: self.link_port.load(std::sync::atomic::Ordering::Relaxed),
        };
        config.validate().map_err(anyhow::Error::msg)?;
        let mut child = Command::new("pkexec")
            .arg(HELPER_PATH)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("no se pudo ejecutar pkexec: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped");
        let mut line = serde_json::to_string(&config)?;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        let (ready_tx, ready_rx) = watch::channel(None);
        let stdout = child.stdout.take().expect("piped");
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                this.on_helper_line(&line, &ready_tx);
            }
            this.on_helper_exit(&ready_tx).await;
        });
        Ok((child, stdin, ready_rx))
    }

    fn on_helper_line(&self, line: &str, ready: &watch::Sender<Option<Result<(), String>>>) {
        tracing::debug!(line, "kushare-hotspot");
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "ready" => {
                let (channel, band) = rest.split_once(' ').unwrap_or((rest, ""));
                self.update(|st| {
                    st.active = true;
                    st.starting = false;
                    st.error.clear();
                    st.band = format!("{band} · canal {channel}");
                });
                ready.send_replace(Some(Ok(())));
            }
            "clients" => {
                let n = rest.trim().parse().unwrap_or(0);
                self.update(|st| st.clients = n);
            }
            "error" => {
                self.update(|st| st.error = rest.to_string());
                if ready.borrow().is_none() {
                    ready.send_replace(Some(Err(rest.to_string())));
                }
            }
            _ => {}
        }
    }

    async fn on_helper_exit(&self, ready: &watch::Sender<Option<Result<(), String>>>) {
        let code = {
            let mut session = self.session.lock().await;
            let code = match session.as_mut() {
                Some(s) => s.child.wait().await.ok().and_then(|st| st.code()),
                None => None,
            };
            *session = None;
            code
        };
        let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
        // pkexec: 126 = el usuario canceló el diálogo, 127 = no autorizado.
        let reason = match code {
            Some(126) => Some("se canceló la autorización".to_string()),
            Some(127) => Some("polkit no autorizó el punto de acceso".to_string()),
            _ => None,
        };
        if ready.borrow().is_none() {
            ready.send_replace(Some(Err(reason
                .clone()
                .unwrap_or_else(|| "el ayudante terminó".to_string()))));
        }
        self.update(|st| {
            st.active = false;
            st.starting = false;
            st.clients = 0;
            st.band.clear();
            if let Some(reason) = reason {
                st.error = reason;
            }
        });
    }

    /// Apaga el AP (el ayudante lo desmonta al ver cerrado su stdin).
    pub async fn stop(&self) {
        let stdin = {
            let mut session = self.session.lock().await;
            session.as_mut().and_then(|s| s.stdin.take())
        };
        drop(stdin);
        // Esperar a que el ayudante termine (lo recoge `on_helper_exit`, que
        // también quita el puente).
        for _ in 0..50 {
            if self.session.lock().await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
        self.unpin().await;
    }
}

async fn iw(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("iw")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .await
        .map_err(|_| anyhow::anyhow!("falta `iw` (instala el paquete iw)"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// La Wi-Fi del PC (y su canal, si está conectada).
async fn base_wifi() -> anyhow::Result<shared::WifiIface> {
    let ifaces = shared::parse_iw_dev(&iw(&["dev"]).await?);
    shared::base_iface(&ifaces)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("este PC no tiene Wi-Fi"))
}

/// Otra antena de la red a la que está conectado `iface` en un canal en el
/// que la tarjeta sí puede emitir. Prefiere 5 GHz (más rápido) salvo que la
/// de 2,4 GHz tenga bastante mejor señal.
async fn find_alternative(iface: &str, current: Option<u32>) -> anyhow::Result<Option<Antenna>> {
    let list = nmcli(&[
        "-t",
        "-f",
        "ACTIVE,SSID,BSSID,CHAN,SIGNAL",
        "device",
        "wifi",
        "list",
        "ifname",
        iface,
    ])
    .await?;
    let phy = iw(&["phy"]).await.unwrap_or_default();
    Ok(pick_alternative(&list, current, |c| {
        shared::channel_no_ir(&phy, c)
    }))
}

fn pick_alternative(
    nmcli_list: &str,
    current: Option<u32>,
    no_ir: impl Fn(u32) -> bool,
) -> Option<Antenna> {
    let rows: Vec<Vec<String>> = nmcli_list.lines().map(split_terse).collect();
    let ssid = rows
        .iter()
        .find(|r| r.first().is_some_and(|a| a == "yes"))?
        .get(1)?
        .clone();
    if ssid.is_empty() {
        return None;
    }
    rows.iter()
        .filter(|r| r.len() >= 5 && r[1] == ssid)
        .filter_map(|r| {
            Some(Antenna {
                bssid: r[2].clone(),
                channel: r[3].parse().ok()?,
                signal: r[4].parse().ok()?,
            })
        })
        .filter(|a| Some(a.channel) != current && !no_ir(a.channel) && a.signal >= MIN_SIGNAL)
        .max_by_key(|a| a.signal + if a.channel > 14 { 10 } else { 0 })
}

/// Campos de una línea de `nmcli -t` (`:` separa; `\:` y `\\` escapan).
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    fields.last_mut().expect("hay campo").push(n);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().expect("hay campo").push(c),
        }
    }
    fields
}

/// Quita los escapes de `nmcli -g` (`AA\:BB` → `AA:BB`).
fn unescape_terse(value: &str) -> String {
    split_terse(value).join(":")
}

/// `kushare-<nombre del host>` (o del dispositivo si no se puede leer),
/// recortado a los 32 bytes de un SSID.
fn default_ssid(device_name: &str) -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| device_name.trim().to_string());
    let mut ssid = format!("kushare-{host}");
    while ssid.len() > 32 {
        ssid.pop();
    }
    if shared::validate_ssid(&ssid).is_ok() {
        ssid
    } else {
        "kushare".to_string()
    }
}

async fn nmcli(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("nmcli")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .await
        .map_err(|e| {
            anyhow::anyhow!("no se pudo ejecutar nmcli (¿NetworkManager instalado?): {e}")
        })?;
    if !out.status.success() {
        anyhow::bail!(
            "nmcli {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_compatible_antenna() {
        let list = "no:INFINITUM:DC\\:8D\\:B7\\:C4\\:4A\\:00:6:84\n\
                    no:.\\:PC Puma FI\\:.:AA\\:46\\:8D\\:2A\\:F7\\:F4:136:67\n\
                    yes:.\\:PC Puma FI\\:.:AA\\:46\\:8D\\:2A\\:F8\\:3A:36:50\n\
                    no:.\\:PC Puma FI\\:.:A8\\:46\\:9D\\:2A\\:F8\\:25:11:47\n\
                    no:.\\:PC Puma FI\\:.:AA\\:46\\:8D\\:2A\\:F8\\:25:157:44\n";
        // 36 y 136 no sirven (36 «no IR», 136 DFS): queda 157 (5 GHz) frente a 11.
        let no_ir = |c: u32| (36..=48).contains(&c) || (52..=144).contains(&c);
        let alt = pick_alternative(list, Some(36), no_ir).unwrap();
        assert_eq!(alt.bssid, "AA:46:8D:2A:F8:25");
        assert_eq!(alt.channel, 157);
        assert!(pick_alternative(list, Some(36), |_| true).is_none());
        assert_eq!(unescape_terse("AA\\:46"), "AA:46");
    }

    #[test]
    fn default_ssid_fits() {
        assert!(default_ssid("REIMUV15").starts_with("kushare-"));
        assert!(default_ssid(&"ñ".repeat(40)).len() <= 32);
    }
}

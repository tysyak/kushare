//! «Compartir internet» por cable USB (opción avanzada).
//!
//! El teléfono abre una VPN y manda sus paquetes IP por un socket a
//! `127.0.0.1:<puerto>`, que `adb reverse` trae hasta este PC. Aquí van a un
//! túnel `kshusb0` que crea NetworkManager **a nombre del usuario**
//! (`tun.owner`, `ipv4.method shared`): el kernel hace el NAT, dnsmasq da
//! el DNS en 10.111.0.1 y firewalld lo pone en su zona. No hace falta root.
//!
//! Formato en el socket, en ambos sentidos: `[u16 big-endian][paquete IP]`.
//! El teléfono manda antes los 16 bytes del token que recibió por el enlace
//! (así ninguna otra app del teléfono puede usar el puerto de `adb reverse`).

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::watch;

/// Conexión de NetworkManager del túnel.
const NM_CONNECTION: &str = "kushare-usb";
/// Interfaz del túnel.
const TUN: &str = "kshusb0";
/// Dirección del PC en el túnel (el teléfono usa la .2, el DNS es la .1).
const TUN_ADDRESS: &str = "10.111.0.1/30";
/// Cuánto se espera a que el teléfono se conecte (la primera vez Android
/// pide permiso para la VPN).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const TOKEN_LEN: usize = 16;
/// `_IOW('T', 202, int)`.
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;

/// Estado para la GUI, la CLI y la TUI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub active: bool,
    /// Esperando a que el teléfono abra la VPN y se conecte.
    pub starting: bool,
    pub error: String,
}

struct Session {
    peer: String,
    port: u16,
    /// Al ponerlo a `true` la tarea del túnel termina (y limpia).
    cancel: watch::Sender<bool>,
    done: tokio::sync::oneshot::Receiver<()>,
}

pub struct UsbNet {
    status: Mutex<Status>,
    session: tokio::sync::Mutex<Option<Session>>,
    changed: watch::Sender<u64>,
    /// Bytes recibidos por el teléfono (del PC) y enviados por él.
    rx: Arc<AtomicU64>,
    tx: Arc<AtomicU64>,
}

impl UsbNet {
    pub fn new() -> Arc<Self> {
        let this = Arc::new(Self {
            status: Mutex::default(),
            session: tokio::sync::Mutex::new(None),
            changed: watch::channel(0).0,
            rx: Arc::default(),
            tx: Arc::default(),
        });
        // Restos de un daemon que se cerró de golpe.
        tokio::spawn(async {
            let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
        });
        this
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// (estado, bytes hacia el teléfono, bytes desde el teléfono).
    pub fn status(&self) -> (Status, u64, u64) {
        (
            self.status.lock().expect("mutex envenenado").clone(),
            self.rx.load(Ordering::Relaxed),
            self.tx.load(Ordering::Relaxed),
        )
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

    /// Marca «arrancando» antes de que la tarea de [`Self::start`] empiece.
    pub fn mark_starting(&self) {
        self.update(|st| {
            if !st.active {
                st.starting = true;
                st.error.clear();
            }
        });
    }

    /// Prepara el túnel, `adb reverse` y avisa al teléfono. Vuelve cuando
    /// el teléfono se conectó o cuando falla.
    pub async fn start(
        self: &Arc<Self>,
        features: Arc<super::features::Features>,
        peer: String,
    ) -> anyhow::Result<()> {
        let mut session = self.session.lock().await;
        if session.is_some() {
            return Ok(());
        }
        self.update(|st| {
            st.starting = true;
            st.error.clear();
        });
        let prepared = self.prepare(&features, &peer).await;
        let (listener, tun, token, port) = match prepared {
            Ok(p) => p,
            Err(err) => {
                let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
                self.update(|st| {
                    st.starting = false;
                    st.error = err.to_string();
                });
                return Err(err);
            }
        };
        let (cancel, cancel_rx) = watch::channel(false);
        let (done_tx, done) = tokio::sync::oneshot::channel();
        let (ready_tx, mut ready) = watch::channel(None::<Result<(), String>>);
        *session = Some(Session {
            peer: peer.clone(),
            port,
            cancel,
            done,
        });
        drop(session);
        self.rx.store(0, Ordering::Relaxed);
        self.tx.store(0, Ordering::Relaxed);

        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = this.run(listener, tun, token, cancel_rx, &ready_tx).await;
            if let Err(err) = &result {
                tracing::info!(%err, "internet por USB terminado");
                if ready_tx.borrow().is_none() {
                    ready_tx.send_replace(Some(Err(err.to_string())));
                }
            }
            this.cleanup(&features, result.err().map(|e| e.to_string()))
                .await;
            let _ = done_tx.send(());
        });

        loop {
            if let Some(r) = ready.borrow_and_update().clone() {
                return r.map_err(anyhow::Error::msg);
            }
            if ready.changed().await.is_err() {
                anyhow::bail!("el túnel USB se cerró");
            }
        }
    }

    async fn prepare(
        &self,
        features: &super::features::Features,
        peer: &str,
    ) -> anyhow::Result<(TcpListener, AsyncFd<OwnedFd>, [u8; TOKEN_LEN], u16)> {
        let serial = adb_device().await?;
        // Túnel a nombre del usuario, con NAT y DNS de NetworkManager.
        let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
        let uid = unsafe { libc::getuid() }.to_string();
        nmcli(&[
            "connection",
            "add",
            "save",
            "no",
            "type",
            "tun",
            "ifname",
            TUN,
            "con-name",
            NM_CONNECTION,
            "autoconnect",
            "no",
            "tun.mode",
            "tun",
            "tun.owner",
            &uid,
            "ipv4.method",
            "shared",
            "ipv4.addresses",
            TUN_ADDRESS,
            "ipv6.method",
            "ignore",
        ])
        .await?;
        nmcli(&["connection", "up", NM_CONNECTION]).await?;
        let tun = AsyncFd::new(open_tun(TUN)?)?;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let spec = format!("tcp:{port}");
        adb(&["-s", &serial, "reverse", &spec, &spec]).await?;

        let mut token = [0u8; TOKEN_LEN];
        getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
        if let Err(err) = features.send_usb_tether(peer, port, &token) {
            let _ = adb(&["-s", &serial, "reverse", "--remove", &spec]).await;
            return Err(err);
        }
        Ok((listener, tun, token, port))
    }

    /// Espera al teléfono y mueve paquetes hasta que algo se cierre.
    async fn run(
        &self,
        listener: TcpListener,
        tun: AsyncFd<OwnedFd>,
        token: [u8; TOKEN_LEN],
        mut cancel: watch::Receiver<bool>,
        ready: &watch::Sender<Option<Result<(), String>>>,
    ) -> anyhow::Result<()> {
        let accept = async {
            loop {
                let (mut stream, _) = listener.accept().await?;
                let mut got = [0u8; TOKEN_LEN];
                let ok = tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut got))
                    .await
                    .is_ok_and(|r| r.is_ok());
                if ok && got == token {
                    return anyhow::Ok(stream);
                }
                tracing::warn!("conexión al túnel USB sin el token correcto: se ignora");
            }
        };
        let stream = tokio::select! {
            r = tokio::time::timeout(CONNECT_TIMEOUT, accept) => {
                r.map_err(|_| anyhow::anyhow!("el teléfono no se conectó (¿aceptaste la VPN?)"))??
            }
            _ = cancel.changed() => return Ok(()),
        };
        stream.set_nodelay(true)?;
        self.update(|st| {
            st.active = true;
            st.starting = false;
        });
        ready.send_replace(Some(Ok(())));
        tracing::info!("internet por USB conectado");

        let tun = Arc::new(tun);
        let (mut rd, mut wr) = stream.into_split();
        let up = {
            let tun = Arc::clone(&tun);
            let tx = Arc::clone(&self.tx);
            async move {
                // Teléfono → internet.
                let mut buf = vec![0u8; 65535];
                loop {
                    let len = rd.read_u16().await? as usize;
                    rd.read_exact(&mut buf[..len]).await?;
                    write_tun(&tun, &buf[..len]).await?;
                    tx.fetch_add(len as u64, Ordering::Relaxed);
                }
                #[allow(unreachable_code)]
                anyhow::Ok(())
            }
        };
        let down = {
            let rx = Arc::clone(&self.rx);
            async move {
                // Internet → teléfono. Se juntan en un solo envío todos los
                // paquetes que ya estén listos (hasta ~64 KiB): adb penaliza
                // mucho los envíos pequeños.
                let mut batch: Vec<u8> = Vec::with_capacity(BATCH + 65537);
                let mut packet = vec![0u8; 65535];
                loop {
                    let len = read_tun(&tun, &mut packet).await?;
                    push_frame(&mut batch, &packet[..len]);
                    let mut total = len as u64;
                    while batch.len() < BATCH {
                        match try_read_tun(&tun, &mut packet) {
                            Some(len) => {
                                push_frame(&mut batch, &packet[..len]);
                                total += len as u64;
                            }
                            None => break,
                        }
                    }
                    wr.write_all(&batch).await?;
                    batch.clear();
                    rx.fetch_add(total, Ordering::Relaxed);
                }
                #[allow(unreachable_code)]
                anyhow::Ok(())
            }
        };
        tokio::select! {
            r = up => r.map_err(|e| anyhow::anyhow!("el teléfono cerró el túnel ({e})")),
            r = down => r,
            _ = cancel.changed() => Ok(()),
        }
    }

    /// Quita `adb reverse`, el túnel y avisa al teléfono.
    async fn cleanup(&self, features: &super::features::Features, error: Option<String>) {
        let session = self.session.lock().await.take();
        if let Some(s) = &session {
            let spec = format!("tcp:{}", s.port);
            let _ = adb(&["reverse", "--remove", &spec]).await;
            let _ = features.send_usb_tether_stop(&s.peer);
        }
        let _ = nmcli(&["connection", "delete", NM_CONNECTION]).await;
        self.update(|st| {
            st.active = false;
            st.starting = false;
            if let Some(e) = error {
                st.error = e;
            }
        });
    }

    /// El teléfono avisó de cómo está su VPN.
    pub async fn on_phone_state(&self, active: bool, error: String) {
        if !active {
            if !error.is_empty() {
                self.update(|st| st.error = error);
            }
            self.stop().await;
        }
    }

    /// Apaga el internet por USB (si estaba en marcha).
    pub async fn stop(&self) {
        let waiting = {
            let mut session = self.session.lock().await;
            match session.as_mut() {
                Some(s) => {
                    let _ = s.cancel.send(true);
                    Some(std::mem::replace(
                        &mut s.done,
                        tokio::sync::oneshot::channel().1,
                    ))
                }
                None => None,
            }
        };
        if let Some(done) = waiting {
            let _ = tokio::time::timeout(Duration::from_secs(10), done).await;
        }
    }
}

/// Abre la interfaz `name` (ya creada por NetworkManager a nombre del
/// usuario) en modo IP puro, sin cabecera extra.
fn open_tun(name: &str) -> anyhow::Result<OwnedFd> {
    // SAFETY: open/ioctl con argumentos válidos; el fd pasa a ser nuestro.
    unsafe {
        let fd = libc::open(
            c"/dev/net/tun".as_ptr(),
            libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
        );
        if fd < 0 {
            anyhow::bail!(
                "no se pudo abrir /dev/net/tun: {}",
                std::io::Error::last_os_error()
            );
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut ifr: libc::ifreq = std::mem::zeroed();
        for (dst, src) in ifr.ifr_name.iter_mut().zip(name.bytes()) {
            *dst = src as libc::c_char;
        }
        ifr.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
        if libc::ioctl(fd.as_raw_fd(), TUNSETIFF as _, &ifr) < 0 {
            anyhow::bail!(
                "no se pudo abrir el túnel {name}: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(fd)
    }
}

async fn read_tun(tun: &AsyncFd<OwnedFd>, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut guard = tun.readable().await?;
        match guard.try_io(|inner| {
            // SAFETY: lectura en un búfer propio del tamaño indicado.
            let n = unsafe { libc::read(inner.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(r) => return r,
            Err(_would_block) => continue,
        }
    }
}

/// Tamaño a partir del cual se manda lo acumulado hacia el teléfono.
const BATCH: usize = 64 * 1024;

fn push_frame(batch: &mut Vec<u8>, packet: &[u8]) {
    batch.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    batch.extend_from_slice(packet);
}

/// Lee un paquete si ya hay uno esperando (sin bloquear).
fn try_read_tun(tun: &AsyncFd<OwnedFd>, buf: &mut [u8]) -> Option<usize> {
    // SAFETY: lectura no bloqueante (el fd se abrió con O_NONBLOCK).
    let n = unsafe {
        libc::read(
            tun.get_ref().as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    (n > 0).then_some(n as usize)
}

async fn write_tun(tun: &AsyncFd<OwnedFd>, packet: &[u8]) -> std::io::Result<()> {
    loop {
        let mut guard = tun.writable().await?;
        match guard.try_io(|inner| {
            // SAFETY: escritura de un búfer válido.
            let n = unsafe { libc::write(inner.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }) {
            Ok(r) => return r,
            Err(_would_block) => continue,
        }
    }
}

/// El único dispositivo adb autorizado (su número de serie).
async fn adb_device() -> anyhow::Result<String> {
    let out = adb(&["devices"]).await?;
    parse_adb_devices(&out)
}

fn parse_adb_devices(out: &str) -> anyhow::Result<String> {
    let rows: Vec<(&str, &str)> = out
        .lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|l| l.split_once('\t'))
        .collect();
    let ready: Vec<&str> = rows
        .iter()
        .filter(|(_, state)| *state == "device")
        .map(|(serial, _)| *serial)
        .collect();
    match ready.as_slice() {
        [one] => Ok(one.to_string()),
        [] if rows.iter().any(|(_, s)| *s == "unauthorized") => anyhow::bail!(
            "el teléfono no ha autorizado a este PC: acepta el aviso de depuración USB en el teléfono"
        ),
        [] => anyhow::bail!(
            "no hay ningún teléfono por USB: conéctalo con la depuración USB activada"
        ),
        _ => anyhow::bail!("hay varios dispositivos adb conectados: deja solo el teléfono"),
    }
}

async fn adb(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("adb")
        .args(args)
        .output()
        .await
        .map_err(|_| anyhow::anyhow!("falta adb (instala android-tools)"))?;
    if !out.status.success() {
        anyhow::bail!(
            "adb {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn nmcli(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("nmcli")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("no se pudo ejecutar nmcli: {e}"))?;
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
    fn adb_devices_are_parsed() {
        let one = "* daemon started successfully\nList of devices attached\nABC123\tdevice\n\n";
        assert_eq!(parse_adb_devices(one).unwrap(), "ABC123");
        let unauth = "List of devices attached\nABC123\tunauthorized\n";
        assert!(
            parse_adb_devices(unauth)
                .unwrap_err()
                .to_string()
                .contains("autorizado")
        );
        assert!(parse_adb_devices("List of devices attached\n\n").is_err());
        let two = "List of devices attached\nA\tdevice\nB\tdevice\n";
        assert!(
            parse_adb_devices(two)
                .unwrap_err()
                .to_string()
                .contains("varios")
        );
    }

    #[tokio::test]
    async fn framing_roundtrip() {
        // El mismo formato que la VPN del teléfono: [u16 BE][paquete].
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let packet = vec![0x45u8; 1400];
        let mut frame = (packet.len() as u16).to_be_bytes().to_vec();
        frame.extend_from_slice(&packet);
        a.write_all(&frame).await.unwrap();
        let len = b.read_u16().await.unwrap() as usize;
        let mut got = vec![0u8; len];
        b.read_exact(&mut got).await.unwrap();
        assert_eq!(got, packet);
    }
}

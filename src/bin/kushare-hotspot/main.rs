//! `kushare-hotspot`: ayudante root del punto de acceso Wi-Fi («Compartir
//! internet»). Lo lanza el daemon con `pkexec` (acción de polkit
//! `org.kushare.hotspot`, instalado en `/usr/lib/kushare/`) y vive mientras
//! el AP esté encendido.
//!
//! - Lee **una línea JSON** por stdin ([`HelperConfig`]): así la clave no
//!   aparece en la línea de órdenes. Todo se valida otra vez aquí.
//! - Crea la interfaz virtual `kshap0` (modo AP) sobre la Wi-Fi del PC, en
//!   el **mismo canal** en el que esta ya está conectada (las tarjetas que
//!   admiten cliente + AP a la vez suelen exigirlo), y arranca `hostapd`
//!   unido al puente `kshbr0`, que el daemon crea con NetworkManager
//!   (DHCP + NAT).
//! - Informa por stdout, una línea por evento: `ready <canal> <banda>`,
//!   `clients <n>`, `error <texto>`.
//! - Si la Wi-Fi del PC cambia de canal, reinicia `hostapd` en el nuevo.
//! - Cuando se cierra stdin (el daemon lo para o se muere) lo desmonta todo.
//!
//! Solo ejecuta `iw`, `ip` y `hostapd`, buscados en rutas fijas del sistema.

#[allow(dead_code)]
#[path = "../../link/hotspot_shared.rs"]
mod shared;

use shared::{AP_IFACE, BRIDGE, HelperConfig};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const RUN_DIR: &str = "/run/kushare-hotspot";
/// Cada cuánto se mira el canal de la Wi-Fi del PC y los clientes.
const CHECK_EVERY: Duration = Duration::from_secs(10);

enum Event {
    /// stdin cerrado: hay que apagar.
    Stop,
    /// Línea de la salida de hostapd (con su generación: al reiniciarlo
    /// por un cambio de canal, lo que diga el anterior se ignora).
    Hostapd(u32, String),
    /// hostapd terminó.
    HostapdExited(u32),
    Tick,
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Busca un programa en las rutas del sistema (nunca en el PATH heredado).
fn tool(name: &str) -> Result<PathBuf, String> {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("falta `{name}` (instala el paquete {name})"))
}

fn run(program: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("no se pudo ejecutar {}: {e}", program.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} {}: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Tools {
    iw: PathBuf,
    ip: PathBuf,
    hostapd: PathBuf,
    /// Para que NetworkManager no se adueñe de la interfaz del AP.
    nmcli: Option<PathBuf>,
}

/// Canal actual de la Wi-Fi del PC (o el de reserva si no está conectada).
fn current_channel(tools: &Tools, base: &str) -> u32 {
    run(&tools.iw, &["dev"])
        .ok()
        .and_then(|out| {
            shared::parse_iw_dev(&out)
                .into_iter()
                .find(|i| i.name == base)
                .and_then(|i| i.channel)
        })
        .unwrap_or(shared::FALLBACK_CHANNEL)
}

/// MAC de la interfaz virtual: la de la Wi-Fi con el bit de «administrada
/// localmente» y el último byte cambiado, para que no coincidan.
fn ap_mac(base: &str) -> Result<String, String> {
    let raw = std::fs::read_to_string(format!("/sys/class/net/{base}/address"))
        .map_err(|e| format!("no se pudo leer la MAC de {base}: {e}"))?;
    let mut bytes: Vec<u8> = raw
        .trim()
        .split(':')
        .map(|h| u8::from_str_radix(h, 16))
        .collect::<Result<_, _>>()
        .map_err(|_| format!("MAC de {base} ilegible"))?;
    if bytes.len() != 6 {
        return Err(format!("MAC de {base} ilegible"));
    }
    bytes[0] = (bytes[0] | 0x02) & !0x01;
    bytes[5] ^= 0x01;
    Ok(bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":"))
}

fn write_config(cfg: &HelperConfig, channel: u32) -> Result<PathBuf, String> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(RUN_DIR)
        .map_err(|e| format!("no se pudo crear {RUN_DIR}: {e}"))?;
    let path = Path::new(RUN_DIR).join("hostapd.conf");
    // Un cliente con la misma MAC que la Wi-Fi del PC (p. ej. si se clonó la
    // del teléfono para entrar en una red con registro por MAC) confunde a la
    // tarjeta: el router de la red del PC lo expulsa y hostapd se cae. Mejor
    // rechazar a ese cliente.
    let deny = Path::new(RUN_DIR).join("deny.mac");
    let base_mac = std::fs::read_to_string(format!("/sys/class/net/{}/address", cfg.base_iface))
        .unwrap_or_default();
    write_private(&deny, format!("{}\n", base_mac.trim()).as_bytes())?;
    let (mode, _) = shared::band(channel);
    let wide = if mode == "a" {
        "ieee80211n=1\nieee80211ac=1\n"
    } else {
        "ieee80211n=1\n"
    };
    // `ssid2` en hexadecimal: admite cualquier nombre (UTF-8, comillas…)
    // sin riesgo de colar otra línea en el archivo. La clave ya se validó
    // como ASCII imprimible sin saltos de línea.
    let text = format!(
        "interface={AP_IFACE}\nbridge={BRIDGE}\ndriver=nl80211\nssid2={}\nutf8_ssid=1\n\
         hw_mode={mode}\nchannel={channel}\n{wide}wmm_enabled=1\n\
         macaddr_acl=0\ndeny_mac_file={}\n\
         wpa=2\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\nwpa_passphrase={}\n",
        hex(cfg.ssid.as_bytes()),
        deny.display(),
        cfg.password
    );
    write_private(&path, text.as_bytes())?;
    Ok(path)
}

fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("no se pudo escribir {}: {e}", path.display()))?;
    file.write_all(data)
        .map_err(|e| format!("no se pudo escribir {}: {e}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn create_iface(tools: &Tools, base: &str) -> Result<(), String> {
    // Restos de una ejecución anterior que no se pudo limpiar.
    let _ = run(&tools.iw, &["dev", AP_IFACE, "del"]);
    run(
        &tools.iw,
        &["dev", base, "interface", "add", AP_IFACE, "type", "__ap"],
    )?;
    // NetworkManager ve una Wi-Fi nueva, la pasa a modo cliente y wpa_supplicant
    // la ocupa: hostapd ya no podría levantarla («Device or resource busy»).
    if let Some(nmcli) = &tools.nmcli {
        for _ in 0..20 {
            if run(nmcli, &["device", "set", AP_IFACE, "managed", "no"]).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    run(&tools.ip, &["link", "set", AP_IFACE, "down"])?;
    run(&tools.iw, &["dev", AP_IFACE, "set", "type", "__ap"])?;
    let mac = ap_mac(base)?;
    run(&tools.ip, &["link", "set", AP_IFACE, "address", &mac])?;
    Ok(())
}

fn start_hostapd(
    tools: &Tools,
    conf: &Path,
    generation: u32,
    tx: &mpsc::Sender<Event>,
) -> Result<Child, String> {
    let mut child = Command::new(&tools.hostapd)
        .arg(conf)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("no se pudo arrancar hostapd: {e}"))?;
    let stdout = child.stdout.take().expect("piped");
    let tx = tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(Event::Hostapd(generation, line));
        }
        let _ = tx.send(Event::HostapdExited(generation));
    });
    Ok(child)
}

fn stations(tools: &Tools) -> usize {
    run(&tools.iw, &["dev", AP_IFACE, "station", "dump"])
        .map(|out| out.lines().filter(|l| l.starts_with("Station ")).count())
        .unwrap_or(0)
}

/// Para hostapd con SIGTERM (desasocia a los clientes y deja la radio en
/// orden) y solo lo mata a la fuerza si no termina en 3 s.
fn stop_hostapd(child: &mut Option<Child>) {
    let Some(mut c) = child.take() else { return };
    if let Ok(pid) = i32::try_from(c.id()) {
        // SAFETY: kill(2) con el pid de un hijo nuestro aún sin recoger.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        for _ in 0..30 {
            if matches!(c.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = c.kill();
    let _ = c.wait();
}

fn cleanup(tools: &Tools, child: &mut Option<Child>) {
    stop_hostapd(child);
    let _ = run(&tools.iw, &["dev", AP_IFACE, "del"]);
    let _ = std::fs::remove_file(Path::new(RUN_DIR).join("hostapd.conf"));
    let _ = std::fs::remove_file(Path::new(RUN_DIR).join("deny.mac"));
}

fn main() {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        say("error no se recibió la configuración");
        std::process::exit(2);
    }
    let cfg: HelperConfig = match serde_json::from_str(&line) {
        Ok(cfg) => cfg,
        Err(_) => {
            say("error configuración ilegible");
            std::process::exit(2);
        }
    };
    if let Err(err) = cfg.validate() {
        say(&format!("error {err}"));
        std::process::exit(2);
    }
    let tools = match (|| {
        Ok::<_, String>(Tools {
            iw: tool("iw")?,
            ip: tool("ip")?,
            hostapd: tool("hostapd")?,
            nmcli: tool("nmcli").ok(),
        })
    })() {
        Ok(t) => t,
        Err(err) => {
            say(&format!("error {err}"));
            std::process::exit(3);
        }
    };
    if !Path::new("/sys/class/net").join(&cfg.base_iface).exists() {
        say(&format!("error no existe la interfaz {}", cfg.base_iface));
        std::process::exit(2);
    }

    let (tx, rx) = mpsc::channel();
    // stdin: cuando se cierra, se apaga todo.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut sink = String::new();
            let stdin = std::io::stdin();
            while stdin.read_line(&mut sink).map(|n| n > 0).unwrap_or(false) {
                sink.clear();
            }
            let _ = tx.send(Event::Stop);
        });
    }
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while tx.send(Event::Tick).is_ok() {
                std::thread::sleep(CHECK_EVERY);
            }
        });
    }

    let mut child: Option<Child> = None;
    let mut channel = current_channel(&tools, &cfg.base_iface);
    let mut generation = 0u32;
    let start = |channel: u32, generation: u32, child: &mut Option<Child>| -> Result<(), String> {
        stop_hostapd(child);
        if run(&tools.iw, &["phy"]).is_ok_and(|phy| shared::channel_no_ir(&phy, channel)) {
            return Err(shared::no_ir_error(channel));
        }
        create_iface(&tools, &cfg.base_iface)?;
        let conf = write_config(&cfg, channel)?;
        *child = Some(start_hostapd(&tools, &conf, generation, &tx)?);
        Ok(())
    };
    if let Err(err) = start(channel, generation, &mut child) {
        say(&format!("error {err}"));
        cleanup(&tools, &mut child);
        std::process::exit(1);
    }

    let mut ready = false;
    let mut clients = usize::MAX;
    for event in rx {
        match event {
            Event::Stop => break,
            Event::Hostapd(g, _) | Event::HostapdExited(g) if g != generation => {}
            Event::Hostapd(_, line) => {
                if line.contains("AP-ENABLED") {
                    ready = true;
                    say(&format!("ready {channel} {}", shared::band(channel).1));
                } else if line.contains("AP-STA-CONNECTED") || line.contains("AP-STA-DISCONNECTED")
                {
                    let n = stations(&tools);
                    if n != clients {
                        clients = n;
                        say(&format!("clients {n}"));
                    }
                } else if !ready
                    && (line.contains("Could not")
                        || line.contains("Failed")
                        || line.contains("failed"))
                {
                    say(&format!("error hostapd: {}", line.trim()));
                }
            }
            Event::HostapdExited(_) => {
                say("error hostapd se detuvo (¿la tarjeta no admite AP en este canal?)");
                break;
            }
            Event::Tick => {
                let now = current_channel(&tools, &cfg.base_iface);
                if ready && now != channel {
                    // La Wi-Fi del PC se movió: el AP tiene que seguirla.
                    channel = now;
                    ready = false;
                    generation += 1;
                    if let Err(err) = start(channel, generation, &mut child) {
                        say(&format!("error {err}"));
                        break;
                    }
                } else if ready {
                    let n = stations(&tools);
                    if n != clients {
                        clients = n;
                        say(&format!("clients {n}"));
                    }
                }
            }
        }
    }
    cleanup(&tools, &mut child);
}

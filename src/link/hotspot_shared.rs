//! Partes del punto de acceso Wi-Fi («Compartir internet») que comparten el
//! daemon ([`super::hotspot`]) y el ayudante root `kushare-hotspot`
//! (src/bin/kushare-hotspot): validación, lectura de `iw` y el QR. Solo usa
//! std + serde para que el ayudante siga siendo pequeño y fácil de revisar.

use serde::{Deserialize, Serialize};

/// Interfaz virtual en modo AP que crea el ayudante.
pub const AP_IFACE: &str = "kshap0";
/// Puente de NetworkManager (`ipv4.method shared`: DHCP + NAT) al que se
/// une el AP.
pub const BRIDGE: &str = "kshbr0";
/// Ruta del ayudante instalado por el paquete (la única que acepta polkit).
pub const HELPER_PATH: &str = "/usr/lib/kushare/kushare-hotspot";

/// Lo que el daemon le pasa al ayudante por stdin (una línea JSON), para que
/// la clave no aparezca en la línea de órdenes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperConfig {
    /// Interfaz Wi-Fi física sobre la que se crea el AP.
    pub base_iface: String,
    pub ssid: String,
    pub password: String,
}

impl HelperConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_iface(&self.base_iface)?;
        validate_ssid(&self.ssid)?;
        validate_password(&self.password)
    }
}

/// Nombre de red: 1–32 bytes, sin caracteres de control (irían tal cual al
/// archivo de hostapd).
pub fn validate_ssid(ssid: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 {
        return Err("el nombre de la red debe tener entre 1 y 32 bytes".into());
    }
    if ssid.chars().any(char::is_control) {
        return Err("el nombre de la red no puede tener caracteres de control".into());
    }
    Ok(())
}

/// Clave WPA2-PSK: 8–63 caracteres ASCII imprimibles.
pub fn validate_password(password: &str) -> Result<(), String> {
    if !(8..=63).contains(&password.len()) {
        return Err("la clave debe tener entre 8 y 63 caracteres".into());
    }
    if !password.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err("la clave solo admite caracteres ASCII imprimibles (sin acentos ni ñ)".into());
    }
    Ok(())
}

/// Nombre de interfaz de red: `[a-z0-9]`, 1–15 caracteres.
pub fn validate_iface(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 15
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(format!("nombre de interfaz no válido: {name:?}"));
    }
    Ok(())
}

/// Una interfaz de `iw dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiIface {
    pub name: String,
    /// `managed`, `AP`, `P2P-device`…
    pub kind: String,
    /// Canal en uso (solo si está conectada).
    pub channel: Option<u32>,
}

/// Lee la salida de `iw dev`.
pub fn parse_iw_dev(output: &str) -> Vec<WifiIface> {
    let mut ifaces: Vec<WifiIface> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Interface ") {
            ifaces.push(WifiIface {
                name: name.trim().to_string(),
                kind: String::new(),
                channel: None,
            });
        } else if let Some(last) = ifaces.last_mut() {
            if let Some(kind) = line.strip_prefix("type ") {
                last.kind = kind.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("channel ") {
                last.channel = rest.split_whitespace().next().and_then(|c| c.parse().ok());
            }
        }
    }
    ifaces
}

/// La interfaz Wi-Fi de este PC sobre la que montar el AP: la que está en
/// modo cliente (`managed`), preferiblemente conectada.
pub fn base_iface(ifaces: &[WifiIface]) -> Option<&WifiIface> {
    let managed = || {
        ifaces
            .iter()
            .filter(|i| i.kind == "managed" && i.name != AP_IFACE)
    };
    managed()
        .find(|i| i.channel.is_some())
        .or_else(|| managed().next())
}

/// Si `iw list` dice que la tarjeta admite AP a la vez que sigue conectada
/// (una combinación con `managed` y `AP`).
pub fn supports_concurrent_ap(iw_list: &str) -> bool {
    let mut in_combos = false;
    for line in iw_list.lines() {
        let line = line.trim();
        if line.starts_with("valid interface combinations") {
            in_combos = true;
        } else if in_combos {
            if line.starts_with('*') && line.contains("managed") && line.contains("AP") {
                return true;
            }
            if !line.starts_with('*') && !line.starts_with("total") && !line.is_empty() {
                in_combos = false;
            }
        }
    }
    false
}

/// Modo de hostapd y nombre de la banda para un canal.
pub fn band(channel: u32) -> (&'static str, &'static str) {
    if channel <= 14 {
        ("g", "2,4 GHz")
    } else {
        ("a", "5 GHz")
    }
}

/// Si `iw phy` marca `channel` como «no IR»: la tarjeta no puede emitir ahí
/// por su cuenta (normativa), así que no puede ser punto de acceso en él.
pub fn channel_no_ir(iw_phy: &str, channel: u32) -> bool {
    let tag = format!("[{channel}]");
    iw_phy
        .lines()
        .filter(|l| l.contains("MHz") && l.contains(&tag))
        .any(|l| l.contains("no IR") || l.contains("disabled"))
}

/// Error para el usuario cuando el canal no sirve.
pub fn no_ir_error(channel: u32) -> String {
    format!(
        "la Wi-Fi del PC está en el canal {channel}, donde esta tarjeta no puede emitir: \
         conéctala a un punto de acceso en otro canal (2,4 GHz, o 5 GHz 149–165) o usa un adaptador Wi-Fi USB"
    )
}

/// Canal si el PC no está conectado por Wi-Fi: en 2,4 GHz, porque en 5 GHz
/// la radio suele no poder emitir sin una red que le diga el país («no IR»).
pub const FALLBACK_CHANNEL: u32 = 6;

/// Texto del QR de Wi-Fi que entienden las cámaras de Android e iOS.
pub fn wifi_qr(ssid: &str, password: &str) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if matches!(c, '\\' | ';' | ',' | ':' | '"') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
    format!("WIFI:T:WPA;S:{};P:{};;", esc(ssid), esc(password))
}

/// Alfabeto de las claves generadas: sin caracteres que se confunden al
/// leerlos (0/O, 1/l/I).
const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Clave aleatoria de `len` caracteres (con rechazo para no sesgar).
pub fn random_password(len: usize) -> String {
    let mut out = String::with_capacity(len);
    let limit = 256 - (256 % ALPHABET.len());
    while out.len() < len {
        let mut buf = [0u8; 32];
        if getrandom::fill(&mut buf).is_err() {
            continue;
        }
        for b in buf {
            if (b as usize) < limit && out.len() < len {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert!(validate_ssid("kushare-pc").is_ok());
        assert!(validate_ssid("").is_err());
        assert!(validate_ssid(&"x".repeat(33)).is_err());
        assert!(validate_ssid("a\nb").is_err());
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password("1234567").is_err());
        assert!(validate_password("contraseña1").is_err());
        assert!(validate_password("abc\ndefgh").is_err());
        assert!(validate_iface("wlp0s20f3").is_ok());
        assert!(validate_iface("wlan0;rm").is_err());
        assert!(validate_iface("../x").is_err());
    }

    #[test]
    fn iw_dev_is_parsed() {
        let out = "phy#0\n\tUnnamed/non-netdev interface\n\t\twdev 0x2\n\t\ttype P2P-device\n\tInterface wlp0s20f3\n\t\tifindex 3\n\t\tssid SSA_N3\n\t\ttype managed\n\t\tchannel 149 (5745 MHz), width: 80 MHz, center1: 5775 MHz\n\tInterface kshap0\n\t\ttype AP\n";
        let ifaces = parse_iw_dev(out);
        assert_eq!(ifaces.len(), 2);
        assert_eq!(ifaces[0].channel, Some(149));
        assert_eq!(base_iface(&ifaces).unwrap().name, "wlp0s20f3");
        assert_eq!(band(149), ("a", "5 GHz"));
        assert_eq!(band(6), ("g", "2,4 GHz"));
    }

    #[test]
    fn no_ir_channels() {
        let phy = "\t\t\t* 2412.0 MHz [1] (22.0 dBm)\n\t\t\t* 5180.0 MHz [36] (22.0 dBm) (no IR)\n\t\t\t* 5745.0 MHz [149] (22.0 dBm)\n\t\t\t* 5260.0 MHz [52] (disabled)\n";
        assert!(channel_no_ir(phy, 36));
        assert!(channel_no_ir(phy, 52));
        assert!(!channel_no_ir(phy, 149));
        assert!(!channel_no_ir(phy, 1));
    }

    #[test]
    fn concurrent_ap_from_iw_list() {
        let list = "\tvalid interface combinations:\n\t\t * #{ managed } <= 1, #{ AP, P2P-client, P2P-GO } <= 1, #{ P2P-device } <= 1,\n\t\t   total <= 3, #channels <= 1\n\tHT Capability overrides:\n";
        assert!(supports_concurrent_ap(list));
        assert!(!supports_concurrent_ap(
            "\tvalid interface combinations:\n\t\t * #{ managed } <= 1, #{ P2P-device } <= 1,\n"
        ));
    }

    // La ida y vuelta de `wifi_qr` con el parser del escáner está en
    // `link::qr::tests` (este archivo también lo compila el ayudante).

    #[test]
    fn random_passwords() {
        let a = random_password(12);
        assert_eq!(a.len(), 12);
        assert!(validate_password(&a).is_ok());
        assert!(a.bytes().all(|b| ALPHABET.contains(&b)));
        assert_ne!(a, random_password(12));
    }
}

//! Actualizaciones: busca la última versión publicada en GitHub
//! (`releases/latest`), elige el paquete de este sistema (Arch, Debian o
//! Fedora según quién instaló `/usr/bin/kushare`) y lo descarga comprobando
//! su SHA-256 contra el `SHA256SUMS` del release.
//!
//! Solo lee la API pública de GitHub: no manda nada del usuario (el
//! User-Agent es "kushare/<versión>").

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "tysyak/kushare";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Comprobación automática como mucho una vez al día.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
const MAX_PACKAGE: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// (nombre, URL de descarga)
    pub assets: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Última versión publicada de `repo` (bloqueante: llamar con
/// `spawn_blocking`).
pub fn latest(repo: &str) -> anyhow::Result<Release> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let body = agent()
        .get(&url)
        .header("User-Agent", &format!("kushare/{CURRENT}"))
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_to_string()?;
    let gh: GhRelease = serde_json::from_str(&body)?;
    Ok(Release {
        version: gh.tag_name.trim_start_matches('v').to_string(),
        page: gh.html_url,
        assets: gh
            .assets
            .into_iter()
            .map(|a| (a.name, a.browser_download_url))
            .collect(),
    })
}

/// ¿`latest` es posterior a `current`? (x.y.z numérico; lo que no se
/// entiende cuenta como 0).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(latest) > parse(current)
}

/// Formato de paquete con el que está instalado kushare: "arch", "deb" o
/// "rpm"; `None` si no viene de un paquete (compilado a mano).
pub fn package_kind() -> Option<&'static str> {
    let owns = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let bin = "/usr/bin/kushare";
    if owns("pacman", &["-Qo", bin]) {
        Some("arch")
    } else if owns("dpkg", &["-S", bin]) {
        Some("deb")
    } else if owns("rpm", &["-qf", bin]) {
        Some("rpm")
    } else {
        None
    }
}

/// El paquete del release para `kind` (sin los de símbolos de depuración).
pub fn pick_asset<'a>(release: &'a Release, kind: &str) -> Option<&'a (String, String)> {
    let suffix = match kind {
        "arch" => ".pkg.tar.zst",
        "deb" => ".deb",
        "rpm" => ".rpm",
        _ => return None,
    };
    release.assets.iter().find(|(name, _)| {
        name.ends_with(suffix) && !name.contains("-debug") && !name.contains("dbgsym")
    })
}

/// Descarga el paquete `asset` a `dir` y comprueba su SHA-256 con el
/// `SHA256SUMS` del mismo release (bloqueante).
pub fn download(
    release: &Release,
    asset: &(String, String),
    dir: &Path,
) -> anyhow::Result<PathBuf> {
    let (name, url) = asset;
    if name.contains('/') || name.starts_with('.') {
        anyhow::bail!("nombre de paquete sospechoso: {name}");
    }
    let sums_url = release
        .assets
        .iter()
        .find(|(n, _)| n == "SHA256SUMS")
        .map(|(_, u)| u.clone())
        .ok_or_else(|| anyhow::anyhow!("el release no trae SHA256SUMS: no se puede verificar"))?;
    let agent = agent();
    let ua = format!("kushare/{CURRENT}");
    let sums = agent
        .get(&sums_url)
        .header("User-Agent", &ua)
        .call()?
        .body_mut()
        .read_to_string()?;
    let expected = sums
        .lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*').rsplit('/').next() == Some(name))
        .map(|(hash, _)| hash.to_lowercase())
        .ok_or_else(|| anyhow::anyhow!("{name} no aparece en SHA256SUMS"))?;

    let mut body = agent.get(url).header("User-Agent", &ua).call()?.into_body();
    let mut reader = body.as_reader().take(MAX_PACKAGE);
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;
    let got = Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != expected {
        anyhow::bail!("el SHA-256 de {name} no coincide: descarga corrupta o manipulada");
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.part"));
    std::fs::write(&tmp, &data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Orden para instalar `path` con privilegios (vía `pkexec`), según el formato.
pub fn install_command(kind: &str, path: &Path) -> Option<Vec<String>> {
    let p = path.to_string_lossy().to_string();
    let cmd: Vec<&str> = match kind {
        "arch" => vec!["pkexec", "pacman", "-U", "--noconfirm"],
        "deb" => vec!["pkexec", "apt-get", "install", "-y"],
        "rpm" if Path::new("/usr/bin/dnf").exists() => vec!["pkexec", "dnf", "install", "-y"],
        "rpm" if Path::new("/usr/bin/zypper").exists() => {
            vec!["pkexec", "zypper", "--non-interactive", "install"]
        }
        "rpm" => vec!["pkexec", "rpm", "-U"],
        _ => return None,
    };
    let mut cmd: Vec<String> = cmd.into_iter().map(str::to_string).collect();
    cmd.push(p);
    Some(cmd)
}

/// Estado de las comprobaciones del daemon (con caché en disco para no
/// preguntar a GitHub en cada arranque).
pub struct Updates {
    state_path: PathBuf,
    state: std::sync::Mutex<UpdateState>,
}

#[derive(Default, Serialize, Deserialize)]
struct UpdateState {
    /// Segundos UNIX de la última comprobación.
    checked_at: u64,
    release: Option<Release>,
    /// Última versión de la que se avisó (para avisar una sola vez).
    notified: String,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

impl Updates {
    pub fn open(cache_dir: &Path) -> Self {
        let state_path = cache_dir.join("update.json");
        let state = std::fs::read(&state_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            state_path,
            state: std::sync::Mutex::new(state),
        }
    }

    fn save(&self, state: &UpdateState) {
        if let Ok(json) = serde_json::to_vec(state) {
            let _ = std::fs::write(&self.state_path, json);
        }
    }

    /// La última versión publicada (de la caché si se miró hace menos de
    /// un día y no se fuerza).
    pub async fn check(&self, force: bool) -> anyhow::Result<Release> {
        {
            let state = self.state.lock().expect("mutex envenenado");
            if !force
                && unix_now().saturating_sub(state.checked_at) < CHECK_EVERY.as_secs()
                && let Some(r) = &state.release
            {
                return Ok(r.clone());
            }
        }
        let release = tokio::task::spawn_blocking(|| latest(REPO)).await??;
        let mut state = self.state.lock().expect("mutex envenenado");
        state.checked_at = unix_now();
        state.release = Some(release.clone());
        self.save(&state);
        Ok(release)
    }

    /// ¿Hay que avisar de `version`? (solo la primera vez).
    pub fn should_notify(&self, version: &str) -> bool {
        let mut state = self.state.lock().expect("mutex envenenado");
        if state.notified == version {
            return false;
        }
        state.notified = version.to_string();
        self.save(&state);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v0.4.0", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.0", "0.3.1"));
    }

    #[test]
    fn picks_the_right_package() {
        let r = Release {
            assets: [
                "kushare-debug-0.4.0-1-x86_64.pkg.tar.zst",
                "kushare-0.4.0-1-x86_64.pkg.tar.zst",
                "kushare_0.4.0-1_amd64.deb",
                "kushare-0.4.0-1.x86_64.rpm",
                "SHA256SUMS",
            ]
            .iter()
            .map(|n| (n.to_string(), format!("https://x/{n}")))
            .collect(),
            ..Default::default()
        };
        assert_eq!(
            pick_asset(&r, "arch").unwrap().0,
            "kushare-0.4.0-1-x86_64.pkg.tar.zst"
        );
        assert_eq!(
            pick_asset(&r, "deb").unwrap().0,
            "kushare_0.4.0-1_amd64.deb"
        );
        assert_eq!(
            pick_asset(&r, "rpm").unwrap().0,
            "kushare-0.4.0-1.x86_64.rpm"
        );
        assert!(pick_asset(&r, "snap").is_none());
    }
}

//! Traducciones de kushare (GUI, bandeja y avisos del daemon).
//!
//! Los textos viven en archivos JSON, uno por idioma (`locales/<idioma>.json`),
//! con claves agrupadas en objetos anidados:
//!
//! ```json
//! { "gui": { "nav": { "home": "Inicio" } } }
//! ```
//!
//! que se consultan con la clave aplanada: `t("gui.nav.home")`. Los valores
//! pueden llevar marcadores `{nombre}`, que rellena [`tf`].
//!
//! **Idioma**: `KUSHARE_LANG`, o si no `LC_ALL` / `LC_MESSAGES` / `LANG`
//! (`es_MX.UTF-8` → `es`). Lo que falte en ese idioma sale en español
//! ([`FALLBACK`]), y una clave que no existe en ningún sitio se muestra tal
//! cual (así se nota enseguida).
//!
//! **Añadir un idioma sin recompilar**: copiar `locales/es.json` a
//! `~/.local/share/kushare/locales/<idioma>.json` (o
//! `/usr/share/kushare/locales/`, o el directorio de `KUSHARE_LOCALE_DIR`) y
//! traducir los valores. Para incluirlo en el binario, añadirlo a [`BUILTIN`].

use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Idioma de respaldo y de referencia (el que siempre está completo).
pub const FALLBACK: &str = "es";

/// Idiomas incluidos en el binario.
pub const BUILTIN: &[(&str, &str)] = &[("es", include_str!("../locales/es.json"))];

type Table = HashMap<String, &'static str>;

struct Catalog {
    language: String,
    primary: Table,
    fallback: Table,
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// Aplana un JSON anidado a claves con puntos. Los valores no-cadena se
/// ignoran (no deberían existir).
fn flatten(prefix: &str, value: &serde_json::Value, out: &mut Table) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        serde_json::Value::String(s) => {
            // Una sola vez por proceso: se "filtra" para devolver &'static str.
            out.insert(prefix.to_string(), Box::leak(s.clone().into_boxed_str()));
        }
        _ => {}
    }
}

/// Lee un catálogo JSON. `None` si no es JSON válido.
pub fn parse(json: &str) -> Option<HashMap<String, String>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut table = Table::new();
    flatten("", &value, &mut table);
    Some(table.into_iter().map(|(k, v)| (k, v.to_string())).collect())
}

fn load_table(json: &str) -> Table {
    let mut table = Table::new();
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(value) => flatten("", &value, &mut table),
        Err(err) => eprintln!("kushare-i18n: catálogo inválido: {err}"),
    }
    table
}

/// Idioma pedido por el entorno ("es", "en"…), sin región ni codificación.
fn requested_language() -> String {
    ["KUSHARE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|v| !v.is_empty())
        .map(|v| {
            v.split(['_', '.', '@', '-'])
                .next()
                .unwrap_or_default()
                .to_lowercase()
        })
        .filter(|l| !l.is_empty() && l != "c" && l != "posix")
        .unwrap_or_else(|| FALLBACK.to_string())
}

/// Directorios donde buscar `<idioma>.json` fuera del binario.
fn locale_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("KUSHARE_LOCALE_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    {
        dirs.push(data.join("kushare/locales"));
    }
    dirs.push(PathBuf::from("/usr/share/kushare/locales"));
    dirs
}

fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        let language = requested_language();
        let builtin = |lang: &str| BUILTIN.iter().find(|(l, _)| *l == lang).map(|(_, j)| *j);
        let fallback = load_table(builtin(FALLBACK).expect("el español siempre está incluido"));
        let external = locale_dirs()
            .into_iter()
            .map(|d| d.join(format!("{language}.json")))
            .find_map(|p| std::fs::read_to_string(p).ok());
        let primary = match (external, builtin(&language)) {
            (Some(json), _) => load_table(&json),
            (None, Some(json)) if language != FALLBACK => load_table(json),
            _ => Table::new(),
        };
        Catalog {
            language,
            primary,
            fallback,
        }
    })
}

/// Idioma activo (el pedido, aunque falten textos y se use el respaldo).
pub fn language() -> &'static str {
    &catalog().language
}

/// Texto de `key` en el idioma activo (o en español; o la clave misma).
pub fn t(key: &str) -> &'static str {
    let c = catalog();
    c.primary
        .get(key)
        .or_else(|| c.fallback.get(key))
        .copied()
        .unwrap_or_else(|| Box::leak(key.to_string().into_boxed_str()))
}

/// Como [`t`], rellenando los marcadores `{nombre}` con `args`.
pub fn tf(key: &str, args: &[(&str, &(dyn Display + Sync))]) -> String {
    fill(t(key), args)
}

fn fill(template: &str, args: &[(&str, &(dyn Display + Sync))]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn spanish_catalog_is_valid_and_fills_placeholders() {
        let es = parse(BUILTIN[0].1).expect("es.json válido");
        assert!(!es.is_empty());
        assert_eq!(
            fill("Hola, {nombre}: {n}", &[("nombre", &"Ana"), ("n", &3)]),
            "Hola, Ana: 3"
        );
    }

    /// Todas las claves que usa el código (`t("…")` / `tf("…"`) existen en
    /// es.json, y es.json no tiene claves que nadie usa.
    #[test]
    fn every_key_used_in_the_code_exists_in_spanish() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut used = BTreeSet::new();
        let mut stack = vec![root.join("src"), root.join("crates/kushare-tray/src")];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .flatten()
                        .map(|e| e.path()),
                );
            } else if path.extension().is_some_and(|e| e == "rs") {
                let src = std::fs::read_to_string(&path).unwrap();
                for marker in ["t(", "tf("] {
                    for (i, _) in src.match_indices(marker) {
                        // Que no sea parte de otro identificador (p. ej. `print(`).
                        let before = src[..i].chars().last();
                        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            continue;
                        }
                        // rustfmt puede dejar la clave en la línea siguiente.
                        let rest = src[i + marker.len()..].trim_start();
                        let Some(rest) = rest.strip_prefix('"') else {
                            continue;
                        };
                        if let Some(end) = rest.find('"') {
                            used.insert(rest[..end].to_string());
                        }
                    }
                }
            }
        }
        let es = parse(BUILTIN[0].1).unwrap();
        let missing: Vec<_> = used.iter().filter(|k| !es.contains_key(*k)).collect();
        assert!(missing.is_empty(), "faltan en es.json: {missing:?}");
        let unused: Vec<_> = es.keys().filter(|k| !used.contains(*k)).collect();
        assert!(unused.is_empty(), "claves de es.json sin usar: {unused:?}");
    }
}

#!/usr/bin/env bash
# Sube la versión de kushare y, si está al lado, de kushare-android, en
# TODOS los sitios (así la GUI, el .deb y el .rpm no se quedan atrás):
#
#   scripts/bump-version.sh 0.4.1
#
# kushare:          Cargo.toml ([workspace.package]), PKGBUILD, Cargo.lock
# kushare-android:  app/build.gradle.kts (versionName y versionCode + 1),
#                   rust/Cargo.toml
# Después: poner fecha a la sección del CHANGELOG y hacer commit.
set -euo pipefail

new="${1:?uso: $0 X.Y.Z}"
[[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "versión inválida: $new" >&2; exit 1; }
root="$(cd "$(dirname "$0")/.." && pwd)"
android="$root/../kushare-android"

# Solo la versión de [workspace.package] (no las de las dependencias).
sed -i "/^\[workspace.package\]/,/^\[/ s/^version = \"[^\"]*\"/version = \"$new\"/" "$root/Cargo.toml"
sed -i "s/^pkgver=.*/pkgver=$new/; s/^pkgrel=.*/pkgrel=1/" "$root/PKGBUILD"
(cd "$root" && cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null)
grep -A3 '^\[workspace.package\]' "$root/Cargo.toml" | grep -q "version = \"$new\"" \
  || { echo "no se pudo cambiar Cargo.toml" >&2; exit 1; }
echo "kushare → $new"

if [[ -d "$android" ]]; then
  gradle="$android/app/build.gradle.kts"
  code=$(sed -n 's/.*versionCode = \([0-9]*\).*/\1/p' "$gradle")
  sed -i "s/versionCode = $code/versionCode = $((code + 1))/; s/versionName = \"[^\"]*\"/versionName = \"$new\"/" "$gradle"
  sed -i "0,/^version = \"[^\"]*\"/s//version = \"$new\"/" "$android/rust/Cargo.toml"
  echo "kushare-android → $new (versionCode $((code + 1)))"
fi

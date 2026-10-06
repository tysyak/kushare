# Maintainer: Cristian Romero Andrade <mascrit@gmail.com>
pkgname=kushare
pkgver=0.1.0
pkgrel=1
pkgdesc="Cliente nativo open-source de Quick Share (Nearby Share) para Linux"
arch=('x86_64' 'aarch64')
url="https://github.com/mascrit/kushare"
license=('GPL-3.0-or-later')
# dbus: tray (StatusNotifierItem) y notificaciones vía zbus (puro Rust,
#   pero el propio bus de sesión debe estar corriendo).
# bluez: descubrimiento BLE vía bluer/BlueZ.
# wayland: cliente Wayland usado por tray-icon/rfd para detectar handles
#   de ventana; en un escritorio X11 puro esta dependencia sigue
#   satisfaciéndose porque el paquete `wayland` solo aporta la librería
#   cliente, no un compositor.
depends=('dbus' 'bluez' 'wayland')
optdepends=('yazi: selector de archivos en kushare-tui (pantalla Enviar)')
makedepends=('cargo' 'git')
source=("$pkgname-$pkgver.tar.gz::https://github.com/mascrit/kushare/archive/refs/tags/v$pkgver.tar.gz")
sha256sums=('SKIP')

prepare() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  cargo build --release --locked
}

check() {
  cd "$pkgname-$pkgver"
  # Solo los crates verificables sin hardware Android ni sesión de
  # escritorio real (proto/crypto/transfer); ver /README.md.
  cargo test --release --locked \
    -p kushare-proto -p kushare-crypto -p kushare-transfer -p kushare-core
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/kushare" "$pkgdir/usr/bin/kushare"
  install -Dm755 "target/release/kushare-tui" "$pkgdir/usr/bin/kushare-tui"
  install -Dm644 "packaging/arch/kushare.desktop" "$pkgdir/usr/share/applications/kushare.desktop"
  install -Dm644 "packaging/common/kushare.svg" "$pkgdir/usr/share/icons/hicolor/scalable/apps/kushare.svg"
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "NOTICE" "$pkgdir/usr/share/licenses/$pkgname/NOTICE"
}

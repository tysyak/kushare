# Maintainer: Cristian Romero Andrade <mascrit@gmail.com>
pkgname=kushare
pkgver=0.6.1
pkgrel=1
pkgdesc="Archivos y notificaciones entre tu PC Linux y tu teléfono (Quick Share, enlace con kushare-android)"
arch=('x86_64' 'aarch64')
url="https://github.com/tysyak/kushare"
license=('GPL-3.0-or-later')
# dbus: tray (StatusNotifierItem) y notificaciones vía zbus (puro Rust,
#   pero el propio bus de sesión debe estar corriendo).
# bluez: descubrimiento BLE vía bluer/BlueZ.
# wayland: cliente Wayland usado por tray-icon/rfd para detectar handles
#   de ventana; en un escritorio X11 puro esta dependencia sigue
#   satisfaciéndose porque el paquete `wayland` solo aporta la librería
#   cliente, no un compositor.
# !lto: el LTO de makepkg deja el código C de `ring` (TLS del actualizador) como
# bitcode de GCC que lld no sabe enlazar ("undefined symbol: ring_core_…").
options=('!lto')
depends=('dbus' 'bluez' 'wayland' 'libxkbcommon')
optdepends=('yazi: selector de archivos en kushare-tui (pantalla Enviar)'
            'zenity: responder notificaciones del teléfono si el servidor no tiene inline-reply'
            'kdialog: alternativa a zenity'
            'xdg-desktop-portal: selector de archivos de la GUI y del tray')
makedepends=('cargo' 'git')
source=("$pkgname-$pkgver.tar.gz::https://github.com/tysyak/kushare/archive/refs/tags/v$pkgver.tar.gz")
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
    -p kushare-proto -p kushare-crypto -p kushare-transfer -p kushare-core -p kushare-link
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/kushare" "$pkgdir/usr/bin/kushare"
  install -Dm755 "target/release/kushare-tui" "$pkgdir/usr/bin/kushare-tui"
  install -Dm755 "target/release/kushare-gui" "$pkgdir/usr/bin/kushare-gui"
  # Lanzador del menú (abre la GUI) y autoarranque del daemon al iniciar sesión.
  install -Dm644 "packaging/common/kushare.desktop" "$pkgdir/usr/share/applications/kushare.desktop"
  install -Dm644 "packaging/common/kushare-autostart.desktop" "$pkgdir/etc/xdg/autostart/kushare.desktop"
  # Alternativa con systemd (no se activa sola): systemctl --user enable --now kushare
  install -Dm644 "packaging/common/kushare.service" "$pkgdir/usr/lib/systemd/user/kushare.service"
  install -Dm644 "README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 "FAQ.md" "$pkgdir/usr/share/doc/$pkgname/FAQ.md"
  install -Dm644 "SECURITY.md" "$pkgdir/usr/share/doc/$pkgname/SECURITY.md"
  install -Dm644 "CHANGELOG.md" "$pkgdir/usr/share/doc/$pkgname/CHANGELOG.md"
  install -Dm644 "packaging/common/kushare.svg" "$pkgdir/usr/share/icons/hicolor/scalable/apps/kushare.svg"
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "NOTICE" "$pkgdir/usr/share/licenses/$pkgname/NOTICE"
}

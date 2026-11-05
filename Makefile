.PHONY: build test fmt clippy package-arch package-deb package-rpm package-deb-inner package-rpm-inner package-all clean clean-debug clean-release clean-packages

build:
	cargo build --release --locked

test:
	cargo test --workspace --locked

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets --locked -- -D warnings

# Arch Linux: corre makepkg dentro de un contenedor archlinux:latest vía
# podman, para no instalar toolchains/paquetes de sistema en el host.
#
# makepkg NO corre en la raíz del repo: su $$srcdir por defecto es ./src,
# que choca con el src/ del crate (así se perdió src/ una vez). Se copia
# solo el PKGBUILD a un directorio de trabajo propio del contenedor
# (/tmp/makepkg, desechable) y al final se copia de vuelta únicamente el
# .pkg.tar.zst. Así tampoco hace falta chown sobre el repo montado (con
# podman rootless eso deja archivos del host con el dueño de un subuid).
package-arch:
	podman run --rm -v "$(CURDIR)":/build:Z docker.io/library/archlinux:latest \
	  bash -euc "pacman -Sy --noconfirm --needed base-devel git rust dbus bluez wayland && \
	    useradd -m builder && \
	    install -d -o builder /tmp/makepkg && \
	    install -o builder -m 644 /build/PKGBUILD /tmp/makepkg/PKGBUILD && \
	    su builder -c 'cd /tmp/makepkg && makepkg -f --noconfirm --skipchecksums' && \
	    cp /tmp/makepkg/*.pkg.tar.zst /build/"

# Debian/.deb y Fedora/.rpm: se construyen dentro de contenedores propios
# con cargo-deb / cargo-generate-rpm (ver packaging/debian, packaging/rpm).
package-deb:
	podman build -t kushare-deb-builder -f packaging/debian/Containerfile .
	podman run --rm -v "$(CURDIR)":/src:Z kushare-deb-builder

package-deb-inner:
	# target/release se limpia primero: si el árbol del host ya tiene un
	# binario de release compilado en OTRO contenedor (p. ej. Fedora), el
	# fingerprint de cargo lo daría por válido y este paso empaquetaría
	# un binario ajeno en vez de recompilar dentro de Debian.
	rm -rf target/release
	cargo build --release --locked
	cargo deb --no-build

package-rpm:
	podman build -t kushare-rpm-builder -f packaging/rpm/Containerfile .
	podman run --rm -v "$(CURDIR)":/src:Z kushare-rpm-builder

package-rpm-inner:
	rm -rf target/release
	cargo build --release --locked
	cargo generate-rpm

package-all: package-arch package-deb package-rpm

# Limpieza granular: solo los artefactos de un perfil de build, sin tocar
# las dependencias ya compiladas del otro perfil (evita recompilar todo).
clean-debug:
	rm -rf target/debug

clean-release:
	rm -rf target/release target/debian target/generate-rpm

# Artefactos de empaquetado que quedan en la raíz del repo (el .pkg.tar.zst
# que copia package-arch) y en target/.
# NUNCA borrar ./src aquí: es el código fuente del binario kushare, no un
# residuo de makepkg (package-arch ya no corre makepkg en la raíz).
clean-packages:
	rm -f ./*.pkg.tar.zst ./*.src.tar.gz
	rm -rf ./pkg
	rm -rf target/debian target/generate-rpm

# Limpieza total: todo lo anterior más el resto de target/ (deps, incremental,
# fingerprints) vía cargo clean.
clean: clean-packages
	cargo clean

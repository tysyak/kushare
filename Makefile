.PHONY: build test fmt clippy package-arch package-deb package-rpm package-deb-inner package-rpm-inner package-all clean clean-debug clean-release clean-packages

build:
	cargo build --release --locked

test:
	cargo test --workspace --locked

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets --locked -- -D warnings

# Mirrors de Arch para el contenedor: por defecto, los 5 más rápidos de la
# mirrorlist del host (packaging/arch/mirrors-rapidos.sh, unos 4 s). Para
# forzar uno: make package-arch ARCH_MIRROR=https://...
ARCH_MIRROR ?=
ARCH_FALLBACK = https://geo.mirror.pkgbuild.com
# Caché de pacman del host: se monta en solo lectura para no volver a bajar
# lo que el PC ya tiene (base-devel, rust…). Los crates de cargo quedan en
# el volumen de podman `kushare-arch-cargo` entre una release y otra.
PACMAN_HOST_CACHE ?= /var/cache/pacman/pkg

# Arch Linux: corre makepkg dentro de un contenedor archlinux:latest vía
# podman, para no instalar toolchains/paquetes de sistema en el host.
#
# makepkg NO corre en la raíz del repo: su $$srcdir por defecto es ./src,
# que choca con el src/ del crate (así se perdió src/ una vez). Se copia
# solo el PKGBUILD a un directorio de trabajo propio del contenedor
# (/tmp/makepkg, desechable) y al final se copia de vuelta únicamente el
# .pkg.tar.zst. Así tampoco hace falta chown sobre el repo montado (con
# podman rootless eso deja archivos del host con el dueño de un subuid).
# Certificados de confianza del host, montados (solo lectura) en los
# contenedores: así funcionan también en redes con proxy HTTPS cuyo
# certificado raíz se añadió al sistema (las descargas de pacman, crates.io
# y GitHub pasan por él). Vacío = no se monta nada.
HOST_CA ?= $(firstword $(wildcard /etc/ca-certificates/extracted/tls-ca-bundle.pem /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt))
ca_mount = $(if $(HOST_CA),-v "$(HOST_CA)":$(1):ro)

package-arch:
	@if [ -n "$(ARCH_MIRROR)" ]; then echo 'Server = $(ARCH_MIRROR)/$$repo/os/$$arch'; \
	  else packaging/arch/mirrors-rapidos.sh /etc/pacman.d/mirrorlist $(ARCH_FALLBACK); fi > target/arch-mirrorlist
	podman run --rm -v "$(CURDIR)":/build:Z \
	  $$([ -d "$(PACMAN_HOST_CACHE)" ] && echo -v "$(PACMAN_HOST_CACHE)":/var/cache/pacman/host:ro) \
	  -v kushare-arch-cargo:/cargo \
	  $(call ca_mount,/etc/ca-certificates/extracted/tls-ca-bundle.pem) \
	  docker.io/library/archlinux:latest \
	  bash -euc "cp /build/target/arch-mirrorlist /etc/pacman.d/mirrorlist && \
	    sed -i 's/^#\?ParallelDownloads.*/ParallelDownloads = 10/' /etc/pacman.conf && \
	    pacman -Syu --noconfirm --needed --disable-download-timeout \
	      --cachedir /var/cache/pacman/pkg --cachedir /var/cache/pacman/host \
	      base-devel git rust dbus bluez wayland libxkbcommon && \
	    useradd -m builder && chown builder /cargo && \
	    install -d -o builder /tmp/makepkg && \
	    install -o builder -m 644 /build/PKGBUILD /tmp/makepkg/PKGBUILD && \
	    su builder -c 'cd /tmp/makepkg && CARGO_HOME=/cargo makepkg -f --noconfirm --skipchecksums' && \
	    cp /tmp/makepkg/*.pkg.tar.zst /build/"

# Debian/.deb y Fedora/.rpm: se construyen dentro de contenedores propios
# con cargo-deb / cargo-generate-rpm (ver packaging/debian, packaging/rpm).
package-deb:
	podman build -t kushare-deb-builder -f packaging/debian/Containerfile .
	podman run --rm -v "$(CURDIR)":/src:Z \
	  $(call ca_mount,/etc/ssl/certs/ca-certificates.crt) kushare-deb-builder

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
	podman run --rm -v "$(CURDIR)":/src:Z \
	  $(call ca_mount,/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem) kushare-rpm-builder

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

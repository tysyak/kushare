.PHONY: build test fmt clippy package-arch package-deb package-rpm package-deb-inner package-rpm-inner package-all clean

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
package-arch:
	podman run --rm -v "$(CURDIR)":/build:Z -w /build docker.io/library/archlinux:latest \
	  bash -c "pacman -Sy --noconfirm --needed base-devel git rust dbus bluez wayland && \
	    useradd -m builder && chown -R builder /build && \
	    su builder -c 'makepkg -f --noconfirm --skipchecksums'"

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

clean:
	cargo clean
	rm -f ./*.pkg.tar.zst
	rm -rf target/debian target/generate-rpm

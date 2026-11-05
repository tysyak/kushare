# kushare

Cliente nativo open-source del protocolo Quick Share (antes Nearby Share) para Linux, escrito en Rust.

Ver [`CLAUDE.md`](./CLAUDE.md) para la especificación completa del proyecto.

## Estado

Scaffold inicial funcional:

- **`crates/kushare-proto`** — mensajes Protocol Buffers del protocolo, vendorizados desde `google/nearby` y `google/ukey2` (ver [`NOTICE`](./NOTICE)).
- **`crates/kushare-crypto`** — handshake UKEY2 y cifrado por mensaje (SecureMessage/D2D), implementado sobre RustCrypto puro Rust.
- **`crates/kushare-core`** — tipos compartidos (`Device`, `TransferEvent`, `Settings`, ...).
- **`crates/kushare-discovery`** — descubrimiento por mDNS (funcional) y BLE real (`bluer`, feature `ble`): anuncia y escanea kushare-a-kushare, y **detecta** un Android real con Quick Share abierto (UUID `0xFEF3`, confirmado con captura real — ver `crates/kushare-discovery/src/ble.rs`). Lo que no logra es lo inverso: un Android real no reconoce el anuncio de kushare como un peer válido (se intentó imitar el formato capturado y falló — ver `examples/android_mimicry_advertise.rs`).
- **`crates/kushare-transfer`** — framing, sesión con handshake UKEY2, envío (TX) y recepción (RX) de archivos, genérico sobre el transporte: TCP (`Receiver`/`Sender`) y Bluetooth RFCOMM clásico (`bt::BtService`, feature `bluetooth`, con `bluer`) comparten el mismo código. El transporte RFCOMM es interoperable con una app Android real (ver [`kushare-android`](https://github.com/mascrit/kushare-android), que implementa el mismo protocolo sobre `BluetoothSocket` nativo) — probado en ambos sentidos con hash SHA-256 idéntico.
- **`crates/kushare-tray`** — icono de bandeja (`tray-icon` con backend `ksni`, sin GTK) y notificaciones D-Bus (`zbus`).
- **`kushare-tui`** (`src/bin/kushare-tui/`) — interfaz de terminal con dos pantallas, "Enviar" y "Recibir", al estilo de la app Quick Share de Android (ver más abajo).

Varias partes del protocolo (ver comentarios `TODO(android-validation)` en el código) se basan en documentación pública de implementaciones de referencia (`Martichou/rquickshare`, `grishka/NearDrop`) y en capturas propias contra un Android real, pero no están completamente confirmadas byte a byte.

## Compilar y probar

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Todos los tests (incluyendo un test de integración de extremo a extremo que hace un handshake UKEY2 real y transfiere un archivo por loopback TCP) pasan sin necesitar hardware Android.

## Correr

```sh
cargo run -- --device-name "mi-portátil"
```

Esto levanta el icono de bandeja, empieza a escuchar transferencias entrantes y se anuncia por mDNS como `_FC9F5ED42C8A._tcp.local.`.

También hay un modo de envío directo por CLI, sin GUI, útil para probar dos instancias entre sí:

```sh
cargo run -- send archivo.txt 127.0.0.1:PUERTO   # el puerto lo imprime la instancia receptora al arrancar
```

## Bluetooth (RFCOMM)

Transporte alternativo a TCP/mDNS, útil cuando los dos dispositivos no tienen conectividad IP entre sí (aislamiento de clientes en WiFi, hotspots que bloquean multicast, firewalls...). Usa el mismo handshake UKEY2 y framing, solo cambia el transporte (ver `crates/kushare-transfer/src/bt.rs`). Requiere que los dispositivos ya estén **emparejados** por Bluetooth de antemano (`bluetoothctl pair <MAC>`).

El modo normal (`cargo run`, sin subcomando) ya escucha por Bluetooth además de TCP, sin nada extra que hacer. Para enviar:

```sh
cargo run -- send-bt archivo.txt AA:BB:CC:DD:EE:FF   # dirección Bluetooth (MAC) del receptor
```

**Limitación de BlueZ, no de kushare**: solo puede haber **un** proceso con el perfil RFCOMM de kushare registrado a la vez en el mismo equipo (`org.bluez.Error.AlreadyExists: UUID already registered` si se intenta un segundo). Si el daemon de fondo (`cargo run` sin subcomando) ya está corriendo con recepción Bluetooth activa, hay que cerrarlo antes de poder usar `send-bt` desde otro proceso en esa misma máquina. No afecta al uso normal, en el que un único proceso de kushare por equipo escucha y envía.

Es interoperable con [`kushare-android`](https://github.com/mascrit/kushare-android): la app Android implementa el mismo UUID de servicio (`b4ac1500-6c92-4e64-8f1e-7e1a0cd4f6a1`) sobre `BluetoothSocket` nativo.

## Firewall

Si el puerto de recepción no es alcanzable desde otro dispositivo (error típico del lado emisor: "No route to host"), casi siempre es el firewall del equipo que corre `kushare`, no el protocolo. Pasa sobre todo con interfaces nuevas (p. ej. tethering USB de un teléfono) que el firewall mete por defecto en una zona/perfil restrictivo.

Hay que abrir el puerto TCP en el que escucha la instancia receptora (lo imprime al arrancar; por defecto el SO elige uno libre, o fija uno con `--port`).

**firewalld** (Fedora, y lo que usa este equipo de desarrollo):

```sh
# averiguar en qué zona cayó la interfaz
firewall-cmd --get-active-zones

# abrir el puerto en esa zona (reemplazar <zona> y <puerto>)
sudo firewall-cmd --zone=<zona> --add-port=<puerto>/tcp          # temporal, no sobrevive un reinicio
sudo firewall-cmd --zone=<zona> --add-port=<puerto>/tcp --permanent && sudo firewall-cmd --reload  # persistente
```

**ufw** (Debian/Ubuntu):

```sh
sudo ufw allow <puerto>/tcp
```

## `kushare-tui`

Interfaz de terminal con dos pantallas, al estilo de la app Quick Share de Android:

```sh
cargo run --bin kushare-tui                        # menú: Enviar / Recibir
cargo run --bin kushare-tui -- --receive            # entra directo a "Recibir"
cargo run --bin kushare-tui -- --send="archivo.txt" # entra directo a "Enviar" con ese archivo
```

- **Recibir**: nombre de dispositivo editable (`e`), visibilidad por mDNS activable/desactivable (`v`), y un log en vivo de la actividad de transferencia.
- **Enviar**: si no se pasó `--send`, ofrece elegir el archivo con [`yazi`](https://yazi-rs.github.io/) (tecla `y`, si está instalado) o escribiendo la ruta a mano (`p`); después lista los dispositivos descubiertos por mDNS en vivo y envía con Enter.

## Empaquetado

```sh
make package-arch   # PKGBUILD, construido dentro de un contenedor archlinux:latest vía podman
make package-deb    # .deb vía cargo-deb, construido dentro de un contenedor Debian vía podman
make package-rpm    # .rpm vía cargo-generate-rpm, construido dentro de un contenedor Fedora vía podman
make package-all    # los tres
```

Ninguno de estos objetivos instala un toolchain de otra distro en el host: cada uno corre dentro de su propio contenedor.

## Validación con un Android real

Los puntos marcados `TODO(android-validation)` en el código son los candidatos más probables a ajustar al probar contra un dispositivo Android real:

```sh
adb logcat | grep -iE "nearby|quickshare"
sudo tcpdump -i any -w captura.pcap port <puerto-anunciado-por-mdns>
cargo run -p kushare-discovery --example ble_dump  # vuelca service_data BLE crudo de dispositivos cercanos
```

Comparar los bytes capturados contra lo que emite/espera este cliente y ajustar los mensajes correspondientes en `kushare-crypto` y `kushare-transfer`.

Ya se hizo una primera ronda contra un Android real (ver `crates/kushare-discovery/src/ble.rs`): se confirmó el UUID de servicio BLE real (`0xFEF3`) y que el nombre visible del dispositivo viaja en claro dentro del `service_data`, pero el resto del payload (probablemente una prueba criptográfica atada a la cuenta de Google) sigue sin descifrarse — sin eso, kushare puede *detectar* un Android real por BLE pero no aparecer en su lista de dispositivos.

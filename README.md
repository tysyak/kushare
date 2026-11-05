# kushare

Cliente nativo open-source del protocolo Quick Share (antes Nearby Share) para Linux, escrito en Rust, que va creciendo hacia algo al estilo de KDE Connect: además de transferir archivos, mantiene un **enlace persistente** con la app [`kushare-android`](https://github.com/tysyak/kushare-android) para **sincronizar notificaciones** en ambas direcciones (ver [Enlace persistente y notificaciones](#enlace-persistente-y-notificaciones)).

¿Problemas? Ejecuta `kushare doctor` y mira el [FAQ](./FAQ.md) (firewall, redes que aíslan dispositivos, KDE Connect, arranque automático…). Cambios por versión en el [CHANGELOG](./CHANGELOG.md).

## Instalar

Los paquetes (Arch `.pkg.tar.zst`, Debian/Ubuntu `.deb`, Fedora `.rpm`) están en los [releases](https://github.com/tysyak/kushare/releases) e instalan:

- `kushare` (daemon + CLI), `kushare-gui` (interfaz gráfica) y `kushare-tui`;
- el lanzador **kushare** en el menú de aplicaciones (abre la GUI);
- el **daemon arranca solo al iniciar sesión** (`/etc/xdg/autostart/kushare.desktop`). En Hyprland/Sway sin uwsm: `exec-once = kushare`, o el servicio de usuario `systemctl --user enable --now kushare` (ver [FAQ](./FAQ.md#el-daemon-de-kushare-no-está-corriendo)).

Después de instalar, abre los puertos del enlace en el firewall si tienes uno (`kushare doctor` te da el comando exacto).

## Interfaz gráfica

`kushare-gui` tiene lo mismo que la app Android: estado del enlace, dispositivos (emparejar por red o Bluetooth con PIN, activar cada dirección de notificaciones, filtro por app, enviar archivos), ajustes de tema (Sistema, Claro, Oscuro, OLED) y diagnóstico. No hace red por su cuenta: controla al daemon por D-Bus.

## Estado

Scaffold inicial funcional:

- **`crates/kushare-proto`** — mensajes Protocol Buffers del protocolo, vendorizados desde `google/nearby` y `google/ukey2` (ver [`NOTICE`](./NOTICE)).
- **`crates/kushare-crypto`** — handshake UKEY2 y cifrado por mensaje (SecureMessage/D2D), implementado sobre RustCrypto puro Rust.
- **`crates/kushare-core`** — tipos compartidos (`Device`, `TransferEvent`, `Settings`, ...).
- **`crates/kushare-discovery`** — descubrimiento por mDNS (funcional) y BLE real (`bluer`, feature `ble`): anuncia y escanea kushare-a-kushare, y **detecta** un Android real con Quick Share abierto (UUID `0xFEF3`, confirmado con captura real — ver `crates/kushare-discovery/src/ble.rs`). Lo que no logra es lo inverso: un Android real no reconoce el anuncio de kushare como un peer válido (se intentó imitar el formato capturado y falló — ver `examples/android_mimicry_advertise.rs`).
- **`crates/kushare-transfer`** — framing, sesión con handshake UKEY2, envío (TX) y recepción (RX) de archivos, genérico sobre el transporte: TCP (`Receiver`/`Sender`) y Bluetooth RFCOMM clásico (`bt::BtService`, feature `bluetooth`, con `bluer`) comparten el mismo código. El transporte RFCOMM es interoperable con una app Android real (ver [`kushare-android`](https://github.com/tysyak/kushare-android), que implementa el mismo protocolo sobre `BluetoothSocket` nativo) — probado en ambos sentidos con hash SHA-256 idéntico.
- **`crates/kushare-tray`** — icono de bandeja (`tray-icon` con backend `ksni`, sin GTK) y notificaciones D-Bus (`zbus`): mostrar con acciones/icono/respuesta, y **capturar** las de otras apps (`monitor.rs`, vía `BecomeMonitor`).
- **`crates/kushare-dbus`** — cliente D-Bus del daemon (`org.kushare.Daemon`), compartido por la CLI y la GUI.
- **`kushare-gui`** (`src/bin/kushare-gui/`) — interfaz gráfica (iced).
- **`crates/kushare-link`** — enlace persistente kushare ⇄ kushare: identidad Ed25519, emparejado con PIN, reconexión automática, notificaciones, filtros por app y configuración sincronizada. Sin dependencias de Linux de escritorio: lo usa también la app Android.
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

Esto levanta el icono de bandeja, empieza a escuchar transferencias entrantes y se anuncia por mDNS como `_FC9F5ED42C8A._tcp.local.`. También abre el [enlace persistente](#enlace-persistente-y-notificaciones) (TCP 47816 y RFCOMM) y reconecta solo con los dispositivos emparejados. Solo puede correr un daemon por sesión (el segundo falla al registrar `org.kushare.Daemon` en D-Bus).

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

BlueZ solo admite **un** registro del perfil RFCOMM de kushare por equipo (`org.bluez.Error.AlreadyExists: UUID already registered` si se intenta un segundo). Por eso, si el daemon está corriendo, `send-bt` no registra nada: le pide el envío por D-Bus y el daemon lo hace con su perfil (el resultado llega como notificación). Sin daemon, `send-bt` registra el perfil él mismo y muestra el progreso. El perfil de transferencias y el del [enlace persistente](#enlace-persistente-y-notificaciones) tienen UUID distintos y conviven: se puede enviar un archivo mientras el enlace de notificaciones sigue activo por Bluetooth.

Es interoperable con [`kushare-android`](https://github.com/tysyak/kushare-android): la app Android implementa el mismo UUID de servicio (`b4ac1500-6c92-4e64-8f1e-7e1a0cd4f6a1`) sobre `BluetoothSocket` nativo.

## Enlace persistente y notificaciones

El daemon (`kushare` sin subcomando) mantiene un enlace con los dispositivos emparejados (hoy, la app Android) y sincroniza sus notificaciones:

- **Ambas direcciones**, cada una activable por separado (*recibir sus notificaciones* / *enviarle las mías*) **desde cualquiera de los dos lados**: el cambio se sincroniza.
- **Filtro por app** en cada dirección.
- **Descartar sincronizado**: cerrar una notificación en un lado la quita del otro.
- **Acciones y respuesta** sobre las notificaciones del teléfono (p. ej. responder un mensaje desde el PC). Si el servidor de notificaciones tiene `inline-reply` (KDE) se responde dentro de la notificación; si no (GNOME, mako, dunst, quickshell…), el botón abre un diálogo de `zenity` o `kdialog`. En sentido contrario no es posible: en Linux un proceso no puede pulsar los botones de la notificación de otra app.
- **Iconos** de la app (o la foto del contacto) en ambos sentidos.

### Emparejar

```sh
kushare scan                 # busca dispositivos kushare en la red (15 s; no hay escaneo continuo)
kushare pair <id|nombre>     # empareja con uno encontrado: ambos lados muestran un PIN de 6 dígitos
kushare pair AA:BB:CC:DD:EE:FF   # o por Bluetooth, con un dispositivo ya vinculado (bluetoothctl pair)
```

El PIN aparece también como notificación con *Aceptar*/*Rechazar*, y en el teléfono como diálogo. Hay que confirmarlo en **los dos** lados. Desde el teléfono se puede emparejar igual (pantalla *Dispositivos*).

Si la red Wi-Fi **aísla a los clientes** entre sí (habitual en redes de oficina, universidades, hoteles, o en routers Meraki en modo NAT: los dispositivos no se alcanzan ni con `ping`), el escaneo no encuentra nada: hay que emparejar por **Bluetooth**.

### Controlar

```sh
kushare devices                        # emparejados (y por dónde están conectados) y los del último escaneo
kushare notif on|off <dispositivo> [in|out|both]   # in = recibir sus notificaciones, out = enviarle las mías
kushare apps <dispositivo>             # apps vistas y cuáles están bloqueadas
kushare block|unblock <dispositivo> in|out <app>
kushare unpair <dispositivo>           # se olvida en ambos lados
kushare quit                           # cierra el daemon
kushare doctor                         # diagnóstico: puerto, firewall, mDNS, Bluetooth, notificaciones
```

`<dispositivo>` acepta el id, un prefijo del id o el nombre. Todo esto también está en el menú del icono de bandeja (*Dispositivos*). Los subcomandos hablan con el daemon por D-Bus (`org.kushare.Daemon` en el bus de sesión), igual que cualquier otra interfaz que se quiera construir.

### Cómo funciona

- **Protocolo propio**, separado de Quick Share ([`kushare_link.proto`](crates/kushare-proto/proto/kushare_link.proto)): servicio mDNS `_kushare-link._tcp.local.` en el puerto TCP **47816**, y RFCOMM con el UUID `6f3a2d10-8c4e-4b7a-9e21-5d0c7b3e9a42` (canal 20). Va sobre el mismo canal cifrado UKEY2 + SecureMessage que las transferencias, con números de secuencia estrictos (se rechazan mensajes repetidos o reordenados).
- **Identidad y emparejado**: cada dispositivo tiene una clave Ed25519 (`~/.config/kushare/identity.key`, permisos 0600) y su `device_id` se deriva de ella. Al emparejar, el PIN sale del secreto de la sesión UKEY2: si hubiera un intermediario, cada lado vería un PIN distinto. Al reconectar no hay PIN: cada lado firma el secreto de la sesión con su clave y el otro lo verifica contra la guardada (`~/.config/kushare/peers.toml`).
- **Configuración sincronizada**: cada dirección y cada filtro llevan un número de versión; gana el cambio más reciente (y en empate, el del `device_id` menor), así ambos lados convergen al reconectar aunque se cambie algo sin conexión.
- **Reconexión**: automática, por Wi-Fi a la última dirección conocida (se aprende en cada conexión, sin escanear) y si no por Bluetooth, con reintentos cada vez más espaciados.
- **Bajo consumo**: no hay escaneo continuo; el keepalive por Bluetooth es de 4 min (la propia pila Bluetooth detecta si el otro dispositivo se aleja).
- **Convivencia con KDE Connect / GSConnect**: sus notificaciones no se reenvían (son las del propio teléfono, o ecos), y kushare nunca reenvía una notificación que lleve la firma de uno de sus espejos. Usar ambos a la vez provocaba un bucle infinito antes de estas dos protecciones; aun así, si los dos sincronizan notificaciones verás cada una duplicada.

## Firewall

Si el puerto de recepción no es alcanzable desde otro dispositivo (error típico del lado emisor: "No route to host"), casi siempre es el firewall del equipo que corre `kushare`, no el protocolo. Pasa sobre todo con interfaces nuevas (p. ej. tethering USB de un teléfono) que el firewall mete por defecto en una zona/perfil restrictivo.

Hay que abrir el puerto TCP en el que escucha la instancia receptora (lo imprime al arrancar; por defecto el SO elige uno libre, o fija uno con `--port`). Para el enlace persistente, además, el puerto **47816/tcp** (fijo; se cambia con `--link-port`) y **mDNS** (5353/udp) para que `kushare scan` encuentre dispositivos.

**firewalld** (Fedora, y lo que usa este equipo de desarrollo):

```sh
# averiguar en qué zona cayó la interfaz
firewall-cmd --get-active-zones

# abrir el puerto en esa zona (reemplazar <zona> y <puerto>)
sudo firewall-cmd --zone=<zona> --add-port=<puerto>/tcp          # temporal, no sobrevive un reinicio
sudo firewall-cmd --zone=<zona> --add-port=<puerto>/tcp --permanent && sudo firewall-cmd --reload  # persistente

# enlace persistente (cada opción en su propia llamada: algunas versiones no admiten
# --add-service y --add-port juntos)
sudo firewall-cmd --zone=<zona> --permanent --add-service=mdns
sudo firewall-cmd --zone=<zona> --permanent --add-port=47816/tcp
sudo firewall-cmd --reload
```

**ufw** (Debian/Ubuntu):

```sh
sudo ufw allow <puerto>/tcp
sudo ufw allow 47816/tcp && sudo ufw allow 5353/udp   # enlace persistente
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

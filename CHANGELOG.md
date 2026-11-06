# Changelog

Todos los cambios notables de kushare (cliente de escritorio). Formato basado en
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/); versionado
[SemVer](https://semver.org/lang/es/).

## [Sin publicar]

## [0.3.0] - 2026-11-06

### Añadido
- **Batería**: la del teléfono se ve en la bandeja, la GUI y `kushare status`,
  con aviso al bajar del 15 %; la del portátil se manda al teléfono (en un
  sobremesa no se manda nada).
- **Multimedia**: los reproductores del teléfono (sesiones de Android:
  YouTube, TIDAL, Spotify…) se controlan desde el PC, y los del PC (MPRIS:
  mpd, navegadores, clientes de música…) desde el teléfono; anterior,
  reproducir/pausar, siguiente y *Pausar todo*.
- **Salida de audio**: el teléfono ve y elige la salida del PC
  (PipeWire/PulseAudio vía `pactl`, mueve también lo que ya suena); el PC ve
  la salida del teléfono.
- **Modo de timbre** del teléfono (sonido, vibración, silencio) desde el PC.
- **Llamadas**: aviso de llamada entrante con *Contestar*/*Rechazar*, y
  *Colgar*/*Silenciar micrófono* durante la llamada; **audio del teléfono en
  el PC** (Bluetooth HFP + A2DP: el PC hace de manos libres y de altavoz).
- **Fotos**: galería del teléfono en la GUI (miniaturas por páginas) y
  descarga del original a *Descargas*; también `kushare photos`.
- GUI: páginas *Teléfono* y *Fotos*. Bandeja: batería, multimedia, salida de
  audio y timbre por dispositivo.
- CLI: `status`, `media`, `audio`, `ringer`, `call`, `phone-audio`, `photos`.
- API D-Bus: `PeerState`, `MediaCommand`, `SetAudioOutput`, `SetRinger`,
  `CallCommand`, `PhoneAudio`, `Photos`, `FetchPhoto` y la señal
  `PeerStateChanged`.
- Protocolo: mensaje `Feature` en `kushare_link.proto` (compatible: los
  peers antiguos lo ignoran).

### Cambiado
- **Esquema de colores** violeta (#9d06d8) con acento rosa, igual en la GUI y
  en Android; OLED con negro puro.
- **Icono de bandeja** propio (el mismo dibujo que el de la app Android), en
  vez del cuadrado azul provisional.
- Los dispositivos también se pueden indicar con una parte del nombre
  (`kushare status armor`).

## [0.2.0] - 2026-11-05

### Añadido
- **`kushare-gui`**: interfaz gráfica (iced) con estado del enlace,
  dispositivos (emparejar por red o Bluetooth con PIN, direcciones de
  notificaciones, filtro por app, enviar archivos), ajustes de tema (Sistema,
  Claro, Oscuro, OLED) y diagnóstico. Controla al daemon por D-Bus.
- **`kushare doctor`**: diagnóstico de puerto, firewall (firewalld y ufw),
  mDNS, Bluetooth, servidor de notificaciones y otros puentes, con el comando
  exacto para arreglar cada problema.
- **FAQ** con los problemas comunes (firewall, redes que aíslan dispositivos,
  KDE Connect, arranque automático…).
- Los paquetes instalan la GUI, su lanzador, el **autoarranque del daemon** al
  iniciar sesión (`/etc/xdg/autostart`) y un servicio de usuario de systemd
  opcional.
- Crate `kushare-dbus` con el contrato D-Bus compartido por CLI y GUI.
- **Enlace persistente con kushare-android** (crate `kushare-link`): identidad
  Ed25519 por dispositivo, emparejado con PIN de 6 dígitos confirmado en ambos
  lados, reconexión automática autenticada sin PIN, keepalive y resolución de
  conexiones duplicadas. Protocolo propio (`_kushare-link._tcp`, puerto 47816,
  RFCOMM `6f3a2d10-…`), separado de Quick Share.
- **Notificaciones sincronizadas** en ambas direcciones: captura de las del
  escritorio (D-Bus `BecomeMonitor`, cualquier servidor de notificaciones),
  espejos de las del teléfono con icono, acciones y respuesta (inline en KDE,
  diálogo zenity/kdialog en el resto), descartar sincronizado, filtro por app y
  cada dirección activable desde cualquiera de los dos lados.
- Emparejado y enlace **por Bluetooth** para redes que aíslan a los clientes.
- API D-Bus `org.kushare.Daemon` y subcomandos `devices`, `scan`, `pair`,
  `unpair`, `notif`, `apps`, `block`, `unblock`, `quit`.
- Submenú *Dispositivos* en la bandeja (direcciones, emparejar, desemparejar,
  buscar, enviar archivo por Bluetooth).
- `send-bt` recuperado: delega en el daemon por D-Bus si está corriendo.
- El daemon recibe archivos también por Bluetooth RFCOMM.

### Cambiado
- Si el puerto 47816 está ocupado, el daemon usa otro libre en vez de fallar
  (propiedad D-Bus `LinkPort`).
- Sin escaneo de red continuo: solo bajo demanda (`kushare scan`).
- Keepalive por Bluetooth de 4 min y reconexión que duerme hasta el próximo
  intento, para gastar menos.
- `SecureChannel` admite secuencia estricta (rechaza mensajes repetidos).
- mDNS genérico por tipo de servicio y nombres de host DNS válidos.

### Corregido
- `make clean` borraba `src/` (el código del binario); `package-arch` ya no
  corre `makepkg` en la raíz del repo ni hace `chown` sobre él.
- Interbloqueo en `BtService` entre la aceptación y `connect()` en el mismo
  proceso.
- Bucle infinito de notificaciones al usar kushare junto a KDE Connect.

## [0.1.0] - 2026-09-22

### Añadido
- Implementación inicial: protocolo Quick Share (UKEY2, SecureMessage),
  transferencia de archivos por TCP y por Bluetooth RFCOMM, descubrimiento por
  mDNS y BLE, icono de bandeja con notificaciones D-Bus, `kushare-tui` y
  empaquetado para Arch, Debian y Fedora vía podman.

[Sin publicar]: https://github.com/tysyak/kushare/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/tysyak/kushare/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tysyak/kushare/releases/tag/v0.2.0
[0.1.0]: https://github.com/tysyak/kushare/commit/cc5ac03

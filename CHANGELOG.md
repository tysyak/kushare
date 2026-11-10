# Changelog

Todos los cambios notables de kushare (cliente de escritorio). Formato basado en
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/); versionado
[SemVer](https://semver.org/lang/es/).

## [Sin publicar]

## [0.5.1] - 2026-11-10

### Cambiado
- La conexión híbrida se ve sola: al conectar y en cada cambio de red (del
  teléfono o del PC) el PC comprueba si alcanza al teléfono por Wi-Fi, y la
  GUI, `kushare devices` («+ datos wifi listo») y el teléfono muestran
  «Bluetooth + Wi-Fi» sin tener que abrir fotos. Al salir de la red vuelve a
  «Bluetooth» en ~2 s; al entrar, «Bluetooth + Wi-Fi» en ~6 s.
- Protocolo: `Feature.lan_status` (el PC le dice al teléfono si el Wi-Fi funciona).

## [0.5.0] - 2026-11-07

### Añadido
- **Conexión híbrida** (por defecto), como Enlace Móvil: el enlace de
  control (notificaciones, llamadas, multimedia, timbre, portapapeles,
  proximidad) va por **Bluetooth**, que en reposo apenas gasta; lo pesado
  (fotos, galería, SMS, contactos, registro de llamadas, avisos, fondo de
  pantalla) abre **a demanda** un enlace de datos por **Wi-Fi** con el
  teléfono, autenticado y cifrado igual, que se cierra solo tras 30 s sin
  uso (la radio Wi-Fi vuelve a ahorro de energía). El teléfono dice su IP
  por Bluetooth: no hace falta escanear la red.
  En casa: 30 miniaturas en 0,5 s (antes ~6 s) y una foto de 3 MB en 0,5 s
  (antes ~70 s). Sin Wi-Fi común, todo sigue por Bluetooth como antes.
- Modo **Solo Bluetooth** (el de la 0.4): `kushare setting hybrid off` o
  *Ajustes → Funciones*.
- **Bloqueo dinámico** (desactivado por defecto): si el teléfono se aleja
  (se pierde su Bluetooth ~30 s), el PC avisa con *Cancelar* y bloquea la
  sesión (`loginctl lock-session`). `kushare setting dynamic-lock on`.
- `kushare devices` muestra el enlace de datos: «conectado por bluetooth +
  datos wifi (…)».

### Cambiado
- El enlace de control se reconecta **primero por Bluetooth** y solo usa
  Wi-Fi si no hay Bluetooth; si quedó por Wi-Fi, vuelve a Bluetooth en
  cuanto se puede (un enlace por Bluetooth siempre sustituye a uno por Wi-Fi).

## [0.4.1] - 2026-11-07

### Añadido
- **Aviso de foto nueva**: cuando el teléfono tiene una foto o captura nueva,
  una notificación del escritorio con la miniatura y los botones *Ver* (la
  original, ya precargada), *Copiar* y *Galería*. Se apaga en *Ajustes →
  Funciones* (`photo-notify`). No avisa de las que ya había al conectar.

### Cambiado
- PKGBUILD sin LTO (`ring` no enlazaba con el LTO de makepkg).

## [0.4.0] - 2026-11-07

"Enlace Móvil" para Linux: el teléfono entero desde el PC.

### Añadido
- **Mensajes (SMS)**: conversaciones del teléfono en la GUI (con búsqueda),
  el hilo en burbujas, responder y empezar conversaciones nuevas; se
  refresca solo al llegar un SMS. CLI: `kushare sms`.
- **Llamadas**: marcador con teclado de teléfono que admite letras
  (1-800-FLORES) y busca contactos por nombre, número o teclas (T9);
  registro de llamadas recientes con volver a llamar o mandar SMS. La
  llamada la hace el teléfono. CLI: `kushare dial`, `calls`, `contacts`.
- **Notificaciones del teléfono** (página *Avisos*): todas las que hay
  ahora en el teléfono, con sus acciones, respuesta rápida y descartar (una
  o todas). CLI: `kushare phone-notifs`.
- **Portapapeles compartido** en ambos sentidos (Wayland con
  *data-control*; X11 como respaldo), con **historial opcional**
  (desactivado por defecto, guardado solo para el usuario). Lo que un gestor
  de contraseñas marca como secreto no se comparte. CLI: `kushare clipboard`.
- **Visor de fotos propio**: la miniatura se agranda al instante y la
  sustituye la original al llegar; abrir con el visor del sistema, copiar
  (como imagen y como archivo), guardar como… y eliminar del teléfono (con
  confirmación). ←/→ y Esc.
- **Fotos por Bluetooth sin esperas**: las miniaturas se guardan en caché y
  el teléfono no las vuelve a mandar; la galería se enseña al instante
  (también sin conexión); las 6 fotos más recientes se **precargan** en
  segundo plano a baja prioridad (trozos pequeños que no frenan las
  notificaciones). Se puede desactivar en *Ajustes*.
- **Fondo de pantalla** del teléfono en la tarjeta de *Teléfono* (o un
  degradado con sus colores si Android no deja leer la imagen).
- **Audio de este PC**: elegir salida y entrada (micrófono, p. ej. para las
  llamadas) desde la GUI. CLI: `kushare pc-audio`.
- **Actualizaciones**: el daemon mira una vez al día si hay versión nueva
  en GitHub y avisa; en *Ajustes → Actualizaciones* se descarga el paquete
  de tu sistema (Arch, Debian o Fedora), se comprueba su SHA-256 contra el
  `SHA256SUMS` del release y se instala con `pkexec`. CLI: `kushare update`.
- **Inglés**: `locales/en.json` (GUI, bandeja y avisos). Selector de idioma
  en *Ajustes* (Sistema, Español, English).
- Ajustes del daemon (`~/.config/kushare/daemon.toml`):
  `clipboard-sync`, `clipboard-history`, `photo-prefetch`, `update-check`.
  CLI: `kushare setting`.
- `scripts/bump-version.sh X.Y.Z`: sube la versión en todos los sitios de
  los dos proyectos.
- Protocolo: mensajes de portapapeles, SMS, registro de llamadas, contactos,
  marcar, notificaciones activas, fondo de pantalla, borrar fotos y
  `CommandResult` (retrocompatible); `have_thumbnails` y descargas de fondo
  en las fotos.

### Cambiado
- La foto original se descarga a la caché de kushare (`~/.cache/kushare/
  photos`, máx. 400 MB); *Guardar como…* o `kushare photos --download`
  la copian donde quieras.
- Miniaturas, fotos, fondos e historial se guardan con permisos 0600/0700.
- Menú lateral más compacto para las páginas nuevas.

### Corregido
- La versión de la 0.3.1 se quedó en 0.3.0 en la GUI y en los nombres del
  `.deb`/`.rpm` (el `Cargo.toml` no se había actualizado).

## [0.3.1] - 2026-11-07

### Añadido
- Fotos en vivo: el teléfono avisa al instante de las nuevas
  (`PhotosChanged`) y aparecen al principio del mosaico con una animación,
  sin recargarlo.
- Traducciones en JSON (`crates/kushare-i18n`, `locales/es.json`).
- FAQ de audio (dirección del audio y códecs Bluetooth).

### Corregido
- Elegir la salida de audio del PC desde el teléfono no hacía nada con
  salidas no disponibles (HDMI sin nada): se ocultan y se comprueba el cambio.
- Procesos `pactl subscribe` huérfanos al cerrar el daemon.
- Menú lateral desalineado en *Dispositivos*; más iconos en la GUI.

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

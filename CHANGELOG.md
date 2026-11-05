# Changelog

Todos los cambios notables de kushare (cliente de escritorio). Formato basado en
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/); versionado
[SemVer](https://semver.org/lang/es/).

## [Sin publicar]

### Añadido
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

[Sin publicar]: https://github.com/tysyak/kushare/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tysyak/kushare/releases/tag/v0.1.0

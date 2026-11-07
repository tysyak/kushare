# Seguridad de kushare (uso doméstico)

Revisión de 0.4.0 de kushare (PC) y kushare-android. Pensada para un
usuario doméstico: qué protege kushare, qué no y qué conviene configurar.

## Lo que está protegido

| Qué | Cómo |
|---|---|
| **Enlace PC ⇄ teléfono** (notificaciones, SMS, llamadas, fotos, portapapeles…) | Cifrado de extremo a extremo (UKEY2 + AES, como Quick Share) y **solo entre dispositivos emparejados**: el emparejado exige confirmar el mismo PIN en los dos lados, y cada reconexión se autentica con una clave Ed25519 propia de cada dispositivo. Un dispositivo no emparejado no puede pedir nada. |
| **Conexión híbrida** | El enlace de datos por Wi-Fi se autentica igual que el de control (UKEY2 + clave Ed25519 del peer emparejado) y va cifrado; solo se abre hacia direcciones de la red local (privadas) que el teléfono comunica por el enlace ya autenticado, y se cierra solo tras 30 s sin uso. |
| **Bloqueo dinámico** | Opcional. Es una comodidad, no sustituye a la contraseña: bloquea al perder el Bluetooth del teléfono, pero no desbloquea nunca. |
| **Recibir archivos por Wi-Fi** (Quick Share) | A ese puerto puede conectarse cualquiera de la red, así que **el PC pregunta antes** (notificación *Aceptar/Rechazar*; sin respuesta en 60 s, rechaza). Se puede desactivar en *Ajustes → Funciones* (`receive-confirm`), mejor no hacerlo en redes ajenas. Los nombres de archivo se limpian para que no escriban fuera de *Descargas*. |
| **Recibir archivos por Bluetooth** | Solo de dispositivos **vinculados** con el PC. |
| **Portapapeles** | Lo que un gestor de contraseñas marca como secreto (`x-kde-passwordManagerHint` en el PC; *sensible* en Android 13+) no se comparte. El historial está **desactivado por defecto**; si se activa, se guarda solo para tu usuario (0600) y se borra al desactivarlo. En Android, *Compartir → Copiar en el PC* pide confirmación, para que otra app no pueda meter texto en el portapapeles del PC. |
| **Borrar fotos desde el PC** | Van a la **papelera** del teléfono (recuperables 30 días) y siempre hay que confirmarlo en el teléfono. |
| **Datos en el PC** | Miniaturas, fotos, fondos de pantalla e historial se guardan en `~/.cache/kushare` y `~/.local/share/kushare` con permisos solo para tu usuario. Los SMS, contactos y el registro de llamadas **no se guardan** en el PC: se piden al teléfono cada vez. |
| **Identidad** | La clave privada no sale de cada dispositivo (en Android, `allowBackup=false`). |
| **Actualizaciones** | Solo desde los releases de GitHub por HTTPS. En el PC se comprueba el SHA-256 contra el `SHA256SUMS` del release y la instalación pide tu contraseña (`pkexec`). En Android el instalador del sistema exige la misma firma que la app instalada. |

## Lo que NO protege (y qué hacer)

- **Cualquier programa de tu sesión del PC puede usar kushare.** La API
  D-Bus (`org.kushare.Daemon`) está en el bus de sesión, como la de KDE
  Connect: un programa malicioso que ya corra con tu usuario podría leer
  tus SMS, mandar SMS o llamar desde el teléfono. Si algo así corre en tu
  sesión, ya tiene acceso a todo tu usuario de todas formas; no instales
  software de origen dudoso.
- **Dar permisos al teléfono es dárselos al PC emparejado.** SMS, llamadas,
  contactos y fotos solo funcionan si los concedes en *Ajustes → Funciones
  del teléfono*; concede solo los que vayas a usar. *Acceso a todos los
  archivos* (para ver el fondo de pantalla) es el permiso más amplio: es
  opcional, y sin él se usan los colores del fondo.
- **El agente Bluetooth de kushare acepta los emparejamientos sin preguntar**
  mientras el PC está *visible* (botón *Hacer visible* de la bandeja).
  Hazlo visible solo mientras vinculas el teléfono.
- **Firewall.** Abre solo `47816/tcp` (enlace) y `5353/udp` (mDNS) y solo en
  tu red de casa (zona `home` de firewalld). En una red pública no hace falta
  abrir nada: el enlace funciona por Bluetooth.
- **Las notificaciones del teléfono se ven en el PC**, también las de bancos
  o códigos de verificación. Bloquea esas apps en *Dispositivos → Apps que
  recibo* (o en el teléfono, *Apps que envío*).
- **Desemparejar** un dispositivo perdido o vendido: `kushare unpair <disp>`
  en el PC (o desde el teléfono). Para empezar de cero, ver `FAQ.md`.

## Informar de un problema

Abre un *issue* en GitHub sin detalles explotables y pide un canal privado.

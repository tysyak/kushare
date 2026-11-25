# Preguntas frecuentes y problemas comunes

Antes de nada, ejecuta el diagnóstico: revisa el daemon, el puerto, el
firewall, mDNS, Bluetooth y el servidor de notificaciones, y te dice el
comando exacto para arreglar cada cosa.

```sh
kushare doctor
```

También está en la GUI: *Ajustes → Diagnóstico*.

---

## El teléfono no encuentra el PC, o falla con "No route to host"

Casi siempre es el **firewall del PC**. kushare necesita:

| Qué | Puerto | Para qué |
|---|---|---|
| Enlace persistente | **47816/tcp** | notificaciones y control con el teléfono |
| mDNS | **5353/udp** | que `kushare scan` y el teléfono se encuentren |
| Transferencias Quick Share | el que muestra `kushare` al arrancar (o `--port`) | recibir archivos por Wi-Fi |

### firewalld (Fedora, openSUSE, Arch con firewalld…)

```sh
# en qué zona está tu interfaz de red
firewall-cmd --get-active-zones

# abrir (cambia <zona>, p. ej. public)
sudo firewall-cmd --zone=<zona> --permanent --add-service=mdns
sudo firewall-cmd --zone=<zona> --permanent --add-port=47816/tcp
sudo firewall-cmd --reload
```

> Algunas versiones de `firewall-cmd` **no admiten** `--add-service` y
> `--add-port` en la misma llamada (`argument --add-port: not allowed with
> argument --add-service`). Usa una llamada por cada opción, como arriba.

### ufw (Debian, Ubuntu, Mint…)

```sh
sudo ufw allow 47816/tcp
sudo ufw allow 5353/udp
```

## `kushare scan` no encuentra nada, y el teléfono y el PC ni se hacen ping

La red Wi-Fi **aísla a los clientes**: cada dispositivo solo puede hablar con
el router, no con los demás. Es habitual en redes de oficina, universidades,
hoteles y routers Cisco Meraki en modo NAT (una pista: puerta de enlace
`10.128.128.128` en una red `/8`). Ningún programa puede saltarse eso por
Wi-Fi.

Solución: **emparejar y conectar por Bluetooth**.

1. Vincula el teléfono con el PC desde los ajustes de Bluetooth del sistema
   (o `bluetoothctl pair <MAC>`).
2. Empareja kushare:
   - desde el teléfono: *Dispositivos → Por Bluetooth → Emparejar* sobre tu PC, o
   - desde el PC: `kushare pair AA:BB:CC:DD:EE:FF` (MAC del teléfono), o
     *Dispositivos → Por Bluetooth* en la GUI.
3. Confirma que el PIN coincide en los dos lados.

A partir de ahí se reconecta solo por Bluetooth.

## Veo las notificaciones duplicadas

Tienes otro puente de notificaciones a la vez: **KDE Connect** o
**GSConnect**. Cada notificación llega por los dos. Desactiva el plugin de
notificaciones de uno de ellos.

kushare ignora lo que publican esos puentes y nunca reenvía un eco de sus
propios espejos, así que ya no se forma un bucle infinito entre ambos (sí
pasaba antes de la 0.2.0).

## "el daemon de kushare no está corriendo"

La GUI, la CLI y el icono de bandeja dependen del daemon (`kushare`). Los
paquetes lo arrancan solo al iniciar sesión con
`/etc/xdg/autostart/kushare.desktop`, que respetan GNOME, KDE, XFCE, etc.

- **Hyprland / Sway** (no leen el autoarranque XDG por sí mismos):
  - añade `exec-once = kushare` a `hyprland.conf`, o
  - si usas **uwsm**, el autoarranque XDG ya funciona; o usa el servicio de
    usuario: `systemctl --user enable --now kushare`.
- **Con systemd**: `systemctl --user enable --now kushare`. No lo combines con
  el autoarranque XDG.
- Desde la GUI: *Inicio → Iniciar* o *Ajustes → Daemon*.

Solo puede correr **un** daemon por sesión. El segundo sale con
`¿ya hay otro daemon de kushare corriendo?`.

## El puerto 47816 está ocupado

El daemon no falla: usa otro puerto libre y lo avisa en el log. El teléfono
lo aprende solo al conectarse, pero el firewall tiene que permitir **ese**
puerto, que además cambia en cada arranque. Libera el 47816 o fija otro con
`kushare --link-port <puerto>` y ábrelo en el firewall. `kushare doctor`
muestra el puerto en uso.

## `org.bluez.Error.AlreadyExists: UUID already registered`

BlueZ solo admite un registro del perfil RFCOMM de kushare por equipo. Desde
la 0.2.0, `kushare send-bt` delega el envío en el daemon si está corriendo,
así que no debería pasar. Si aparece, hay otro proceso de kushare antiguo
abierto: ciérralo (`kushare quit`).

## No puedo responder a un mensaje desde el PC

Responder a una notificación del teléfono requiere:

- un servidor de notificaciones con **inline-reply** (KDE Plasma), o
- `zenity` (o `kdialog`) instalado: el botón *Responder* abre un diálogo.
  Es el caso de GNOME, mako, dunst, swaync, quickshell…

En sentido contrario (pulsar botones de notificaciones del PC desde el
teléfono) no es posible: en Linux un programa no puede activar las acciones
de la notificación de otro.

## Las notificaciones del teléfono no llegan al PC

En el teléfono, comprueba:

1. **Acceso a notificaciones** concedido a kushare (*Ajustes* de la app).
2. kushare **encendido** (botón de los ajustes rápidos) y no **en pausa**
   (en pausa = sin Bluetooth ni Wi-Fi).
3. En *Dispositivos*, *Enviar mis notificaciones* activado para ese PC y la
   app no bloqueada en *Apps que envío*.
4. Opcional: quitar a kushare de la **optimización de batería** si el
   fabricante es agresivo cerrando apps.

En el PC: `kushare devices` debe mostrarlo *conectado* y con *recibir: sí*.

## ¿Dónde quedan los archivos recibidos?

- **PC**: en la carpeta de descargas (`~/Descargas` o `~/Downloads`).
- **Teléfono**: en *Descargas*, con una notificación para abrirlos.

## Quiero olvidar un dispositivo o empezar de cero

- Desemparejar (en los dos lados): `kushare unpair <dispositivo>`, o desde la
  GUI o el teléfono.
- Borrar la identidad y todos los emparejados del PC: cierra el daemon y
  borra `~/.config/kushare/` (`identity.key`, `peers.toml`). Los dispositivos
  tendrán que volver a emparejarse.

## ¿Hacia dónde va el audio: del teléfono al PC o al revés?

Hay dos funciones distintas:

| Función | Dirección | Qué hace |
|---|---|---|
| **Audio del teléfono en el PC** (`kushare phone-audio <disp> on`, *Teléfono → Escuchar aquí*) | **teléfono → PC** | El PC hace de **altavoz** (A2DP) y de **manos libres** (HFP) del teléfono por Bluetooth: la música del teléfono suena en el PC y, en una llamada, oyes con los altavoces del PC y hablas con su micrófono. |
| **Salida de audio** (`kushare audio`, *Salida de audio* en la tarjeta del PC en el teléfono) | ninguna: solo elige altavoz | Elige **por dónde suena cada equipo su propio audio**. Desde el teléfono puedes cambiar la salida del PC (altavoces, HDMI, auriculares…). Al revés solo se ve: Android no deja a una app cambiar la salida de las demás (en el teléfono, *Ajustes → Elegir salida de audio*). |

El audio del PC **no** se manda al teléfono: kushare no convierte el teléfono
en altavoz del PC.

`kushare phone-audio <disp> off` (o *Devolver*) hace que el audio vuelva a
sonar en el teléfono.

## No oigo en el PC la música o las llamadas del teléfono

`kushare phone-audio <dispositivo> on` (o *Teléfono → Escuchar aquí* en la
GUI) conecta los perfiles Bluetooth de audio del teléfono. Si responde solo
"llamadas", o falla:

- En el teléfono: *Ajustes → Bluetooth →* tu PC *→* activa **Audio
  multimedia** y **Llamadas**.
- En el PC, WirePlumber debe tener los roles `a2dp_sink` y `hfp_hf` (vienen
  por defecto; revisa `~/.config/wireplumber/wireplumber.conf.d/` si los
  cambiaste).
- El enlace tiene que conocer la dirección Bluetooth del teléfono: empareja
  kushare por Bluetooth al menos una vez.

### Elijo el PC como salida en el teléfono y se queda "conectando"

El Bluetooth sí se conecta, pero el teléfono no acepta el **códec** que
negoció el PC. Pasó con un Ulefone (MediaTek) y **aptX HD**: el teléfono lo
anuncia, pero su sistema de audio nunca levanta la salida. Solución: que
PipeWire ofrezca solo códecs que cualquier Android soporta. Crea
`~/.config/wireplumber/wireplumber.conf.d/51-kushare-bluez-codecs.conf`:

```
monitor.bluez.properties = {
  bluez5.codecs = [ sbc sbc_xq aac ldac ]
}
```

y reinicia WirePlumber (`systemctl --user restart wireplumber`; el audio se
corta un segundo). Vuelve a elegir el PC en el teléfono. Para comprobar qué
códec se usa: `pactl list short sink-inputs` muestra un flujo
`… (codec AAC)` mientras suena. Ojo: esto aplica a todos los dispositivos
Bluetooth del PC (los auriculares que solo usen aptX pasarán a SBC o AAC).

## El PC no puede contestar llamadas, ver fotos o poner el teléfono en silencio

Son permisos opcionales del teléfono: en kushare, *Ajustes → Funciones del
teléfono*, concede **Llamadas**, **Fotos** y **No molestar**.

## El portapapeles del teléfono no llega al PC

Android solo deja leer el portapapeles a la app que está en primer plano.
Lo que copias en el PC llega al teléfono al instante, pero lo que copias en
el teléfono se manda al PC cuando:

- abres kushare en el teléfono,
- pulsas **Enviar portapapeles** en la notificación de kushare o en *Ajustes*,
- usas el botón de ajustes rápidos **Portapapeles al PC**, o
- compartes un texto con **Copiar en el PC**.

En el PC hace falta un compositor con *data-control* (Hyprland, KDE Plasma,
Sway…). En GNOME se usa X11 (XWayland) como respaldo. Lo que un gestor de
contraseñas marca como secreto no se comparte nunca.

## El PC pregunta antes de recibir un archivo por Wi-Fi

Desde la 0.4.0, a propósito: al receptor de Quick Share por Wi-Fi puede
conectarse cualquiera de la red, así que el PC muestra *Aceptar/Rechazar*
(y rechaza si no contestas en 60 s). Por Bluetooth solo aceptan archivos los
dispositivos vinculados, sin preguntar. En casa puedes desactivarlo:
*Ajustes → Funciones → Preguntar antes de recibir por Wi-Fi*, o
`kushare setting receive-confirm off`. Ver [SECURITY.md](./SECURITY.md).

## No veo SMS, llamadas, contactos o el fondo de pantalla

Son permisos opcionales del teléfono: en kushare, *Ajustes → Funciones del
teléfono*, concede **SMS**, **Registro de llamadas y contactos** y, para el
fondo de pantalla, **acceso a todos los archivos** (sin él, el PC pinta un
degradado con sus colores). Solo se leen MMS de texto como SMS normales: las
imágenes de un MMS no se muestran.

## ¿Cómo actualizo?

El daemon mira una vez al día si hay versión nueva y te avisa. En la GUI,
*Ajustes → Actualizaciones*: descarga el paquete de tu sistema, comprueba su
SHA-256 y lo instala (pide tu contraseña). En la terminal:
`kushare update --download`. En el teléfono, *Ajustes → Actualizaciones*.

## ¿El enlace gasta mucha batería? ¿Por dónde va cada cosa?

Desde la 0.5.0 la conexión es **híbrida**: el enlace permanente va por
Bluetooth (en reposo apenas consume) y lo pesado —fotos, SMS, listas— abre un
enlace por Wi-Fi solo mientras hace falta (se cierra a los 30 s sin uso). Así
la radio Wi-Fi del teléfono no se queda despierta y el Bluetooth no se satura
con transferencias largas. `kushare devices` lo muestra:
«conectado por bluetooth + datos wifi (192.168.0.17:47816)».

Si el teléfono y el PC no están en la misma red (o la red aísla los
dispositivos), todo sigue por Bluetooth. Para forzarlo siempre:
`kushare setting hybrid off` o, en el teléfono, *Ajustes → Conexión híbrida*.

## El bloqueo dinámico no bloquea

Comprueba que esté activado (`kushare setting`) y que tu escritorio atienda
`loginctl lock-session` (en Hyprland, `lock_cmd` en `hypridle.conf`). Solo
cuenta el **Bluetooth**: si el teléfono sigue conectado por Wi-Fi no se
considera cerca ni lejos; se bloquea tras ~30 s sin su Bluetooth.

## La webcam del teléfono no aparece en mis apps

kushare crea la cámara virtual con **v4l2loopback** si está cargado y, si no,
con **PipeWire** (sin root). `kushare doctor` dice cuál tienes.

- PipeWire: instala `gst-plugin-pipewire` y `gst-plugins-bad` (Arch),
  `gstreamer1.0-pipewire` y `gstreamer1.0-plugins-bad` (Debian/Ubuntu) o
  `pipewire-gstreamer` y `gstreamer1-plugins-bad-free` (Fedora). La ven Firefox,
  Chromium, OBS y GNOME; **no** Zoom, Teams ni Discord (solo leen `/dev/video*`).
- v4l2loopback: `sudo modprobe v4l2loopback exclusive_caps=1` (paquete
  `v4l2loopback-dkms`). La ven todas las apps.
- Para forzar uno: `kushare webcam start <disp> --backend pipewire|v4l2`.

## El micrófono del teléfono no aparece o suena entrecortado

Necesita `pactl` (pipewire-pulse o PulseAudio): la entrada se llama
«Micrófono de <teléfono>» (`kushare_mic`). La primera vez el teléfono pide el
permiso de micrófono en su pantalla. Por Bluetooth va a 16 kHz (calidad de
llamada); con el teléfono y el PC en la misma Wi-Fi, a 48 kHz.

## «Compartir internet» no enciende la red

Ejecuta `kushare doctor` (sección *Compartir internet*). Lo habitual:

- **«la Wi-Fi del PC está en el canal N, donde esta tarjeta no puede emitir»**: la red del teléfono tiene que ir en el mismo canal que la del PC, y en ese la tarjeta no puede ser punto de acceso. Si tu red tiene otra antena en un canal válido, kushare la ofrece («Usar la antena del canal…» en la GUI, `A` en la TUI, `kushare hotspot on --switch-antenna`): la Wi-Fi del PC se queda en ella mientras la red esté encendida y al apagarla vuelve a como estaba. Si no hay ninguna, conecta el PC a otra red o usa un adaptador Wi-Fi USB.
- **«se canceló la autorización»**: cerraste el diálogo de polkit. Vuelve a encenderla y escribe tu contraseña.
- **Falta `hostapd` o el ayudante**: instala `hostapd` y reinstala el paquete de kushare.
- **El teléfono se conecta y la Wi-Fi del PC se cae**: el teléfono usa la misma MAC que el PC (pasa si clonaste la MAC del teléfono en el PC para una red con registro). kushare rechaza a ese cliente; en el teléfono pon *Privacidad → MAC aleatoria* para esa red.

## Mando tonos (DTMF) y el menú de voz no reacciona

Android no deja a otras apps entrar en una llamada (salvo a la de teléfono), así
que kushare manda los tonos por **Bluetooth manos libres**: el teléfono los
genera como si pulsaras su teclado, pero Android los corta a ~5 ms y algunos
menús (p. ej. buzones en VoLTE) no los reconocen. Si el manos libres no está
conectado y la llamada se oye en el PC (*Escuchar aquí*), van como sonido. Si
no los reconoce, márcalos en el teléfono.

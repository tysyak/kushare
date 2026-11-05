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

# Instrucciones del Proyecto: Quick Share Linux (quickshare-linux)

## Objetivo Principal

* Desarrollar un cliente nativo open-source del protocolo Quick Share (antes Nearby Share) para Linux.

* Capacidades core: Descubrimiento (BLE/mDNS), Envio y Recepcion de archivos.

* Interfaz: Implementar un icono de bandeja del sistema (System Tray / AppIndicator) persistente para mantener el programa en segundo plano, gestionar transferencias y lanzar notificaciones nativas de escritorio.

* Empaquetado: Crear paquetes nativos comenzando por Arch Linux (PKGBUILD), seguido de Debian (.deb) y Red Hat (.rpm).

* Enrolamiento: Opcional en v1 (soportar transferencias como "dispositivo visible para todos" temporalmente para evitar la autenticacion compleja de cuentas de Google en la fase inicial).

## Stack Tecnologico y Dependencias

* Lenguaje: Rust (Edicion 2021 o superior).

* Concurrencia y Red: Framework `tokio` para programacion asincrona y manejo de Sockets TCP/HTTP.

* Descubrimiento de red: `mdns-sd` o similar para mDNS (LAN) y `bluer` (interfaz de BlueZ) para deteccion de proximidad via Bluetooth LE.

* Criptografia: Implementar el handshake seguro (UKEY2 / OfflineCommunicator) utilizando librerias nativas de Rust como `ring` o `rustls`.

* System Tray e Integracion: Utilizar `tray-icon` para la bandeja del sistema y `zbus` para la comunicacion con D-Bus (necesario para notificaciones nativas e interaccion profunda con el escritorio Linux).

## Flujo de Pruebas con ADB

* Utilizar un dispositivo Android fisico conectado via ADB para validacion.

* Lectura de logs: Usar `adb logcat | grep -i nearby` o `QuickShare` para hacer ingenieria inversa al handshake y ver el motivo de fallo de las conexiones.

* Simulacion de envio: Enviar archivos al telefono usando `adb push` y abrir el menu de compartir nativo hacia el cliente de Linux.

* Captura de paquetes: Usar tcpdump/Wireshark en Linux para analizar el trafico en el puerto especifico que abre Android al compartir.

## Fases de Desarrollo

### 1. Ingenieria Inversa y Protocolo Base

* Implementar el parser de Protocol Buffers (`prost` o `protobuf`) que usa Quick Share para sus mensajes.

* Lograr el Handshake de conexion: Intercambio de llaves criptograficas (UKEY2).

* Establecer la conexion asincrona (Tokio) inicial y mantener el socket abierto.

### 2. Modulo de Recepcion (RX)

* Anunciar el servicio en la red local via mDNS (\_FC9F5ED42C8A.\_tcp.local u otros identificadores de Nearby).

* Aceptar la conexion entrante de Android.

* Guardar el archivo recibido en un directorio temporal (/tmp o \~/Downloads) de forma asincrona.

### 3. Modulo de Envio (TX)

* Escanear la red buscando dispositivos Android visibles via BLE/mDNS.

* Iniciar la peticion de envio (enviar metadatos del archivo).

* Transferir los bytes mediante streams una vez que el Android acepta la solicitud.

### 4. Interfaz de Usuario y Segundo Plano

* Integrar el icono en la bandeja del sistema utilizando hilos separados o canales (mpsc) para no bloquear el runtime asincrono.

* Añadir menus contextuales (Click derecho) en el tray icon: "Enviar archivo", "Hacer visible", "Salir".

* Conectar eventos del protocolo asincrono con notificaciones de escritorio D-Bus (ej. "Archivo recibido de Dispositivo X").

### 5. Empaquetado y Distribucion

* Arch Linux: Crear archivo PKGBUILD estandar. Usar `cargo build --release --locked` en el bloque build().

* Debian: Configurar y utilizar `cargo-deb` para generar el paquete .deb directamente desde Cargo.toml.

* Red Hat: Configurar y utilizar `cargo-generate-rpm` para compilar el paquete .rpm.

## Reglas para el Asistente AI

* Generar codigo modular y limpio, separando la logica de red, criptografia y System Tray en distintos modulos (crates o submodulos de Cargo).

* Usar exclusivamente el ecosistema de Rust (Cargo, Tokio, Prost, Bluer).

* Evitar en lo posible dependencias que requieran compilacion en C puro para facilitar el empaquetado cross-platform dentro de Linux.

* Priorizar la automatizacion: Crear un archivo Makefile o scripts `cargo make` que ejecuten los builds, tests y la generacion de paquetes (.pkg.tar.zst, .deb, .rpm).

* Si algo falla en el protocolo, sugerir comandos de ADB especificos para depurar el error en el dispositivo Android.
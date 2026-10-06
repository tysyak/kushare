//! Mensajes Protocol Buffers del protocolo Quick Share / Nearby Share,
//! generados a partir de los `.proto` vendorizados en `proto/` (ver /NOTICE).
//!
//! prost-build emite un único `mod.rs` que anida cada paquete protobuf como
//! módulo real de Rust siguiendo su nombre punteado (p. ej.
//! `location::nearby::connections`); eso es necesario para que las
//! referencias cruzadas entre .proto (rutas `super::super::...` generadas)
//! resuelvan correctamente. Aquí se reexportan con nombres cortos y estables
//! para el resto del workspace.
include!(concat!(env!("OUT_DIR"), "/mod.rs"));

pub use location::nearby::connections as offline_wire_formats;
pub use location::nearby::proto::sharing as sharing_enums;
pub use nearby::sharing::service::proto as wire_format;
// `securegcm` y `securemessage` ya quedan como módulos top-level tras el
// include! (sus paquetes protobuf no tienen prefijo de puntos).

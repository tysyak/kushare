//! Lista de reproductores (`MediaPlayers`): cuándo merece la pena mandarla.
//!
//! La posición viaja «en el momento de mandar» y el receptor la extrapola
//! mientras suena, así que un mensaje en el que solo avanzó la posición no
//! aporta nada: se omite (cada envío despierta la radio, y por Bluetooth
//! lleva además la carátula). Un salto (`seek`) sí se manda.

use crate::pb;
use std::time::Duration;

/// Margen entre la posición esperada y la nueva antes de considerarla un salto.
pub const POSITION_TOLERANCE_MS: i64 = 1500;

fn playing(p: &pb::MediaPlayer) -> bool {
    p.state == pb::PlaybackState::PlaybackPlaying as i32
}

/// Posición de `p` (enviada hace `elapsed`) ahora mismo, sin pasarse de la duración.
fn position_after(p: &pb::MediaPlayer, elapsed: Duration) -> i64 {
    if !playing(p) {
        return p.position_ms;
    }
    let pos = p.position_ms.saturating_add(elapsed.as_millis() as i64);
    if p.duration_ms > 0 {
        pos.min(p.duration_ms)
    } else {
        pos
    }
}

/// ¿`new` no aporta nada respecto a `old`, enviado hace `elapsed`? Es así si
/// todo es igual salvo la posición, y esta es la que el receptor ya calcula.
pub fn same_players(old: &pb::MediaPlayers, new: &pb::MediaPlayers, elapsed: Duration) -> bool {
    old.players.len() == new.players.len()
        && old.players.iter().zip(&new.players).all(|(o, n)| {
            let fixed = |p: &pb::MediaPlayer| pb::MediaPlayer {
                position_ms: 0,
                ..p.clone()
            };
            fixed(o) == fixed(n)
                && (position_after(o, elapsed) - n.position_ms).abs() <= POSITION_TOLERANCE_MS
        })
}

/// `players` con la posición puesta al día (para reenviar una copia guardada
/// hace `elapsed`, p. ej. al conectar un peer).
pub fn advanced(players: &pb::MediaPlayers, elapsed: Duration) -> pb::MediaPlayers {
    pb::MediaPlayers {
        players: players
            .players
            .iter()
            .map(|p| pb::MediaPlayer {
                position_ms: position_after(p, elapsed),
                ..p.clone()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(state: pb::PlaybackState, position_ms: i64, title: &str) -> pb::MediaPlayers {
        pb::MediaPlayers {
            players: vec![pb::MediaPlayer {
                id: "app#1".into(),
                title: title.into(),
                state: state as i32,
                art: vec![1, 2, 3],
                position_ms,
                duration_ms: 200_000,
                ..Default::default()
            }],
        }
    }

    const PLAY: pb::PlaybackState = pb::PlaybackState::PlaybackPlaying;
    const PAUSE: pb::PlaybackState = pb::PlaybackState::PlaybackPaused;
    const S10: Duration = Duration::from_secs(10);

    #[test]
    fn solo_avanzar_la_posicion_no_se_manda() {
        let old = list(PLAY, 1000, "a");
        assert!(same_players(&old, &list(PLAY, 11_200, "a"), S10));
        // En pausa la posición no avanza.
        assert!(same_players(
            &list(PAUSE, 1000, "a"),
            &list(PAUSE, 1000, "a"),
            S10
        ));
    }

    #[test]
    fn saltos_y_cambios_si_se_mandan() {
        let old = list(PLAY, 1000, "a");
        assert!(!same_players(&old, &list(PLAY, 60_000, "a"), S10), "seek");
        assert!(!same_players(&old, &list(PAUSE, 11_000, "a"), S10), "pausa");
        assert!(
            !same_players(&old, &list(PLAY, 11_000, "b"), S10),
            "otra canción"
        );
        let mut art = list(PLAY, 11_000, "a");
        art.players[0].art = vec![9];
        assert!(!same_players(&old, &art, S10), "otra carátula");
        assert!(!same_players(&old, &pb::MediaPlayers::default(), S10));
    }

    #[test]
    fn la_copia_guardada_se_pone_al_dia() {
        assert_eq!(
            advanced(&list(PLAY, 1000, "a"), S10).players[0].position_ms,
            11_000
        );
        assert_eq!(
            advanced(&list(PAUSE, 1000, "a"), S10).players[0].position_ms,
            1000
        );
        let end = advanced(&list(PLAY, 199_000, "a"), S10);
        assert_eq!(
            end.players[0].position_ms, 200_000,
            "no pasa de la duración"
        );
    }
}

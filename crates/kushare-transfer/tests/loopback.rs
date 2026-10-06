//! Test de integración de extremo a extremo, sin ningún dispositivo
//! Android: levanta un `Receiver` en `127.0.0.1`, conecta un `Sender` en el
//! mismo proceso, hace un handshake UKEY2 real por TCP y transfiere un
//! archivo pequeño, verificando su hash SHA-256 en el destino.

use kushare_core::TransferEvent;
use kushare_transfer::{Receiver, Sender};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn send_file_loopback_end_to_end() {
    let download_dir = tempfile::tempdir().expect("tempdir para el receptor");
    let (rx_events_tx, mut rx_events_rx) = mpsc::channel(16);

    let receiver = Receiver::bind(0, download_dir.path().to_path_buf(), rx_events_tx)
        .await
        .expect("bind receiver");
    let addr = receiver.local_addr().expect("local_addr");

    tokio::spawn(receiver.run());

    let src_dir = tempfile::tempdir().expect("tempdir para el origen");
    let src_path = src_dir.path().join("saludo.txt");
    let contents = b"Hola desde el test de loopback de kushare!".repeat(1000);
    tokio::fs::write(&src_path, &contents)
        .await
        .expect("escribir archivo de origen");

    let (tx_events_tx, _tx_events_rx) = mpsc::channel(16);
    let mut session = Sender::connect(addr).await.expect("connect");
    Sender::send_file(
        &mut session,
        "TEST",
        "kushare-test-sender",
        &src_path,
        tx_events_tx,
    )
    .await
    .expect("send_file");

    let saved_to = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx_events_rx.recv().await.expect("canal de eventos cerrado") {
                TransferEvent::Completed { saved_to, .. } => return saved_to,
                TransferEvent::Failed { reason, .. } => panic!("transferencia falló: {reason}"),
                _ => continue,
            }
        }
    })
    .await
    .expect("timeout esperando TransferEvent::Completed");

    let received = tokio::fs::read(&saved_to)
        .await
        .expect("leer archivo recibido");
    assert_eq!(
        Sha256::digest(&received).as_slice(),
        Sha256::digest(&contents).as_slice(),
        "el hash del archivo recibido no coincide con el original"
    );
}

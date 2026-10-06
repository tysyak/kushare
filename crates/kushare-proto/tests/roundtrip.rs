use kushare_proto::offline_wire_formats::{
    ConnectionRequestFrame, OfflineFrame, V1Frame, connection_request_frame::Medium,
    offline_frame::Version, v1_frame::FrameType,
};
use prost::Message;

#[test]
fn offline_frame_connection_request_roundtrips() {
    let frame = OfflineFrame {
        version: Some(Version::V1 as i32),
        v1: Some(V1Frame {
            r#type: Some(FrameType::ConnectionRequest as i32),
            connection_request: Some(ConnectionRequestFrame {
                endpoint_id: Some("ABCD".to_string()),
                endpoint_name: Some("kushare-test-device".to_string()),
                mediums: vec![Medium::WifiLan as i32, Medium::Ble as i32],
                nonce: Some(42),
                ..Default::default()
            }),
            ..Default::default()
        }),
    };

    let encoded = frame.encode_to_vec();
    assert!(!encoded.is_empty());

    let decoded = OfflineFrame::decode(encoded.as_slice()).expect("decode OfflineFrame");
    assert_eq!(decoded, frame);

    let v1 = decoded.v1.expect("v1 frame present");
    assert_eq!(v1.r#type, Some(FrameType::ConnectionRequest as i32));

    let request = v1.connection_request.expect("connection_request present");
    assert_eq!(request.endpoint_id.as_deref(), Some("ABCD"));
    assert_eq!(
        request.mediums,
        vec![Medium::WifiLan as i32, Medium::Ble as i32]
    );
}

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn connect_json_envelopes_wait_for_complete_bounded_frames() {
    let envelope =
        encode_connect_json_message(&json!({"message": "hello"})).expect("message should encode");
    let split = envelope.len() - 1;
    let mut buffer = envelope[..split].to_vec();
    assert!(
        take_frame(&mut buffer)
            .expect("partial frame should be accepted")
            .is_none()
    );
    buffer.extend_from_slice(&envelope[split..]);
    let Some(ConnectJsonFrame::Message(payload)) =
        take_frame(&mut buffer).expect("complete frame should decode")
    else {
        panic!("expected a message frame");
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&payload).expect("payload should be JSON"),
        json!({"message": "hello"})
    );
    assert!(buffer.is_empty());
}

#[test]
fn connect_json_end_stream_errors_and_oversized_frames_fail_closed() {
    assert!(matches!(
        validate_end_stream(br#"{"error":{"code":"unavailable","message":"retry"}}"#),
        Err(EnvironmentProviderAdapterError::Unavailable { .. })
    ));

    let mut oversized = vec![0];
    oversized.extend_from_slice(&((MAX_MESSAGE_BYTES + 1) as u32).to_be_bytes());
    assert!(matches!(
        take_frame(&mut oversized),
        Err(EnvironmentProviderAdapterError::Internal { .. })
    ));
}

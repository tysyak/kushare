#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("unexpected UKEY2 message type: expected {expected:?}, got {got:?}")]
    UnexpectedMessageType {
        expected: &'static str,
        got: Option<i32>,
    },

    #[error("no supported UKEY2 handshake cipher offered by peer")]
    NoSupportedCipher,

    #[error(
        "UKEY2 commitment mismatch: peer's ClientFinish does not match the commitment sent in ClientInit"
    )]
    CommitmentMismatch,

    #[error("malformed public key")]
    MalformedPublicKey,

    #[error("elliptic curve error: {0}")]
    EllipticCurve(#[from] p256::elliptic_curve::Error),

    #[error("handshake used in the wrong order or state: {0}")]
    InvalidState(&'static str),

    #[error("SecureMessage signature verification failed")]
    SignatureMismatch,

    #[error("missing required field in {0}")]
    MissingField(&'static str),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

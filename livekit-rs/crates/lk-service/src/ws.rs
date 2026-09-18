//! Signal frame encoding over the WebSocket.
//!
//! Ports `pkg/service/wsprotocol.go`. A client may speak protobuf or protojson
//! and the server follows whichever it sees: a binary frame means protobuf, a
//! text frame means JSON, and the answer goes back in the same encoding. The JS
//! SDK sends binary; the JSON path exists for hand-written clients and for
//! debugging, and its unknown-field handling has to match protojson's
//! `DiscardUnknown`, which is why the JSON goes through `pbjson` rather than
//! `prost`'s own support.

use axum::extract::ws::Message;
use lk_proto::livekit::{SignalRequest, SignalResponse};
use prost::Message as _;

use crate::error::{Error, Result};

/// How long between WebSocket pings, from `pingFrequency`.
pub const PING_FREQUENCY_SECONDS: u64 = 10;

/// How long a ping write may take, from `pingTimeout`.
pub const PING_TIMEOUT_SECONDS: u64 = 2;

/// How long a close write may take, from `closeWriteTimeout`.
pub const CLOSE_WRITE_TIMEOUT_SECONDS: u64 = 5;

/// The encoding a connection settled on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    /// Binary frames carrying protobuf. The default, and what every SDK uses.
    #[default]
    Protobuf,
    /// Text frames carrying protojson.
    Json,
}

/// Encodes and decodes signal frames, following the client's encoding.
#[derive(Clone, Debug, Default)]
pub struct SignalCodec {
    encoding: Encoding,
    message_size_limit: i64,
}

impl SignalCodec {
    /// A codec bounding each frame to `message_size_limit` bytes. Zero means
    /// unbounded.
    #[must_use]
    pub fn new(message_size_limit: i64) -> Self {
        Self {
            encoding: Encoding::default(),
            message_size_limit,
        }
    }

    /// The encoding in use, which follows the last frame the client sent.
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Decodes a frame.
    ///
    /// Returns `Ok(None)` for a frame that carries no request: a ping, a pong,
    /// or a close. The Go server logs and ignores those rather than failing the
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BadRequest`] when the payload exceeds the size limit or
    /// does not decode.
    pub fn decode_request(&mut self, message: &Message) -> Result<Option<SignalRequest>> {
        match message {
            Message::Binary(payload) => {
                self.check_size(payload.len())?;
                // a binary frame means the client speaks protobuf, even if an
                // earlier frame was text
                self.encoding = Encoding::Protobuf;
                SignalRequest::decode(payload.as_ref())
                    .map(Some)
                    .map_err(|_| Error::BadRequest("cannot decode signal request"))
            }
            Message::Text(payload) => {
                self.check_size(payload.len())?;
                self.encoding = Encoding::Json;
                serde_json::from_str(payload.as_str())
                    .map(Some)
                    .map_err(|_| Error::BadRequest("cannot decode signal request"))
            }
            Message::Ping(_) | Message::Pong(_) | Message::Close(_) => Ok(None),
        }
    }

    /// Encodes a response in the connection's encoding.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] when the response cannot be serialised,
    /// which only a bug in the message can cause.
    pub fn encode_response(&self, response: &SignalResponse) -> Result<Message> {
        match self.encoding {
            Encoding::Protobuf => Ok(Message::Binary(response.encode_to_vec().into())),
            Encoding::Json => serde_json::to_string(response)
                .map(|json| Message::Text(json.into()))
                .map_err(|err| Error::Internal(format!("cannot encode signal response: {err}"))),
        }
    }

    fn check_size(&self, len: usize) -> Result<()> {
        if self.message_size_limit > 0 && len as i64 > self.message_size_limit {
            return Err(Error::BadRequest("signal message exceeds size limit"));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use lk_proto::livekit::{signal_request, signal_response};

    use super::*;

    fn ping_request() -> SignalRequest {
        SignalRequest {
            message: Some(signal_request::Message::Ping(42)),
        }
    }

    fn pong_response() -> SignalResponse {
        SignalResponse {
            message: Some(signal_response::Message::Pong(42)),
        }
    }

    #[test]
    fn a_binary_frame_selects_protobuf_for_the_answer() {
        let mut codec = SignalCodec::new(0);
        let frame = Message::Binary(ping_request().encode_to_vec().into());
        assert_eq!(codec.decode_request(&frame).unwrap(), Some(ping_request()));
        assert_eq!(codec.encoding(), Encoding::Protobuf);
        assert!(matches!(
            codec.encode_response(&pong_response()).unwrap(),
            Message::Binary(_)
        ));
    }

    #[test]
    fn a_text_frame_selects_json_for_the_answer() {
        let mut codec = SignalCodec::new(0);
        let json = serde_json::to_string(&ping_request()).unwrap();
        let frame = Message::Text(json.into());
        assert_eq!(codec.decode_request(&frame).unwrap(), Some(ping_request()));
        assert_eq!(codec.encoding(), Encoding::Json);

        let Message::Text(answer) = codec.encode_response(&pong_response()).unwrap() else {
            panic!("a json connection must be answered with text frames");
        };
        let decoded: SignalResponse = serde_json::from_str(answer.as_str()).unwrap();
        assert_eq!(decoded, pong_response());
    }

    #[test]
    fn a_client_switching_to_binary_switches_the_answer_back() {
        let mut codec = SignalCodec::new(0);
        let json = serde_json::to_string(&ping_request()).unwrap();
        codec.decode_request(&Message::Text(json.into())).unwrap();
        assert_eq!(codec.encoding(), Encoding::Json);

        codec
            .decode_request(&Message::Binary(ping_request().encode_to_vec().into()))
            .unwrap();
        assert_eq!(codec.encoding(), Encoding::Protobuf);
    }

    #[test]
    fn json_frames_discard_unknown_fields_like_protojson() {
        // protojson unmarshals with DiscardUnknown, so a client sending a field
        // this build does not know must still connect.
        let mut codec = SignalCodec::new(0);
        let frame = Message::Text(r#"{"ping":"42","somethingNew":true}"#.into());
        assert_eq!(codec.decode_request(&frame).unwrap(), Some(ping_request()));
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut codec = SignalCodec::new(8);
        let frame = Message::Binary(vec![0u8; 9].into());
        assert!(codec.decode_request(&frame).is_err());

        // the limit is inclusive, and zero disables it
        let mut codec = SignalCodec::new(0);
        assert!(
            codec
                .decode_request(&Message::Binary(ping_request().encode_to_vec().into()))
                .is_ok()
        );
    }

    #[test]
    fn control_frames_carry_no_request() {
        let mut codec = SignalCodec::new(0);
        assert_eq!(
            codec
                .decode_request(&Message::Ping(Vec::new().into()))
                .unwrap(),
            None
        );
        assert_eq!(
            codec
                .decode_request(&Message::Pong(Vec::new().into()))
                .unwrap(),
            None
        );
        assert_eq!(codec.decode_request(&Message::Close(None)).unwrap(), None);
    }
}

use std::pin::Pin;

use futures::Stream;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::EnvironmentProviderAdapterError;

const ENVELOPE_HEADER_LEN: usize = 5;
const END_STREAM_FLAG: u8 = 0b0000_0010;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub(crate) fn encode_connect_json_message<T>(
    message: &T,
) -> Result<Vec<u8>, EnvironmentProviderAdapterError>
where
    T: Serialize + ?Sized,
{
    let payload =
        serde_json::to_vec(message).map_err(|error| EnvironmentProviderAdapterError::Internal {
            message: format!("failed to encode Connect JSON request: {error}"),
        })?;
    let length =
        u32::try_from(payload.len()).map_err(|_| EnvironmentProviderAdapterError::Internal {
            message: "Connect JSON request is too large".to_string(),
        })?;
    let mut envelope = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
    envelope.push(0);
    envelope.extend_from_slice(&length.to_be_bytes());
    envelope.extend_from_slice(&payload);
    Ok(envelope)
}

pub(crate) fn connect_json_stream<T>(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<T, EnvironmentProviderAdapterError>> + Send>>
where
    T: DeserializeOwned + Send + 'static,
{
    let state = ConnectJsonStreamState {
        response,
        buffer: Vec::new(),
    };
    Box::pin(futures::stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(frame) = take_frame(&mut state.buffer)? {
                match frame {
                    ConnectJsonFrame::Message(payload) => {
                        let message = serde_json::from_slice(&payload).map_err(|error| {
                            EnvironmentProviderAdapterError::Internal {
                                message: format!(
                                    "failed to decode Connect JSON stream message: {error}"
                                ),
                            }
                        })?;
                        return Ok(Some((message, state)));
                    }
                    ConnectJsonFrame::End(payload) => {
                        if !state.buffer.is_empty() {
                            return Err(EnvironmentProviderAdapterError::Internal {
                                message: "Connect JSON stream contains data after EndStream"
                                    .to_string(),
                            });
                        }
                        validate_end_stream(&payload)?;
                        return Ok(None);
                    }
                }
            }

            let chunk = state.response.chunk().await.map_err(|error| {
                EnvironmentProviderAdapterError::Unavailable {
                    message: format!("failed to read Connect JSON stream: {error}"),
                }
            })?;
            let Some(chunk) = chunk else {
                return Err(EnvironmentProviderAdapterError::Unavailable {
                    message: "Connect JSON stream ended without an EndStream envelope".to_string(),
                });
            };
            if state.buffer.len().saturating_add(chunk.len())
                > ENVELOPE_HEADER_LEN + MAX_MESSAGE_BYTES
            {
                return Err(EnvironmentProviderAdapterError::Internal {
                    message: "Connect JSON stream buffer exceeded its limit".to_string(),
                });
            }
            state.buffer.extend_from_slice(&chunk);
        }
    }))
}

struct ConnectJsonStreamState {
    response: reqwest::Response,
    buffer: Vec<u8>,
}

enum ConnectJsonFrame {
    Message(Vec<u8>),
    End(Vec<u8>),
}

fn take_frame(
    buffer: &mut Vec<u8>,
) -> Result<Option<ConnectJsonFrame>, EnvironmentProviderAdapterError> {
    if buffer.len() < ENVELOPE_HEADER_LEN {
        return Ok(None);
    }
    let flags = buffer[0];
    if flags != 0 && flags != END_STREAM_FLAG {
        return Err(EnvironmentProviderAdapterError::Internal {
            message: format!("unsupported Connect JSON envelope flags {flags:#04x}"),
        });
    }
    let length = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;
    if length > MAX_MESSAGE_BYTES {
        return Err(EnvironmentProviderAdapterError::Internal {
            message: format!("Connect JSON envelope exceeds {MAX_MESSAGE_BYTES} bytes"),
        });
    }
    let frame_len = ENVELOPE_HEADER_LEN + length;
    if buffer.len() < frame_len {
        return Ok(None);
    }
    let payload = buffer
        .drain(..frame_len)
        .skip(ENVELOPE_HEADER_LEN)
        .collect();
    Ok(Some(if flags == END_STREAM_FLAG {
        ConnectJsonFrame::End(payload)
    } else {
        ConnectJsonFrame::Message(payload)
    }))
}

#[derive(Deserialize)]
struct ConnectEndStreamResponse {
    #[serde(default)]
    error: Option<ConnectError>,
}

#[derive(Deserialize)]
struct ConnectError {
    code: String,
    #[serde(default)]
    message: String,
}

fn validate_end_stream(payload: &[u8]) -> Result<(), EnvironmentProviderAdapterError> {
    let response: ConnectEndStreamResponse = serde_json::from_slice(payload).map_err(|error| {
        EnvironmentProviderAdapterError::Internal {
            message: format!("failed to decode Connect EndStream envelope: {error}"),
        }
    })?;
    if let Some(error) = response.error {
        let message = if error.message.is_empty() {
            format!("Ona event watch ended with Connect error {}", error.code)
        } else {
            format!(
                "Ona event watch ended with Connect error {}: {}",
                error.code, error.message
            )
        };
        return Err(EnvironmentProviderAdapterError::Unavailable { message });
    }
    Ok(())
}

#[cfg(test)]
#[path = "connect_json_stream_tests.rs"]
mod tests;

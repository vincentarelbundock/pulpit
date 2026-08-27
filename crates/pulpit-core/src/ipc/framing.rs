//! Length-prefixed message framing over a pipe.
//!
//! Both worker protocols move `bincode`-encoded messages behind a four-byte
//! little-endian length. The *shapes* of those messages are deliberately not
//! shared — a render job and a media request have nothing to say to each other
//! — but the envelope around them is one problem, and it is the half where a
//! mistake is a security bug rather than a wrong picture: a length is read
//! from a pipe and used to size an allocation, so it has to be refused
//! *before* anything is allocated.
//!
//! The ceiling is the caller's, because the two protocols legitimately differ:
//! the render worker carries attachments and allows tens of mebibytes, while
//! the media worker's messages are small and a generous limit would only widen
//! what a compromised browser process could ask for.

use std::io::{Read, Write};

use serde::Serialize;

/// Why a message could not be moved across the pipe.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed message: {0}")]
    Malformed(String),
    /// The limit travels with the error rather than being baked into the
    /// message, because each protocol sets its own.
    #[error("message of {bytes} bytes exceeds the {limit} byte limit")]
    TooLarge { bytes: u32, limit: u32 },
    #[error("peer closed the connection")]
    Closed,
    #[error("protocol version mismatch: worker speaks {theirs}, we speak {ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
}

/// Write one length-prefixed message, refusing one larger than `limit`.
pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
    limit: u32,
) -> Result<(), ProtocolError> {
    let encoded = bincode::serialize(message)
        .map_err(|e| ProtocolError::Malformed(format!("encode: {e}")))?;
    let bytes = u32::try_from(encoded.len()).map_err(|_| ProtocolError::TooLarge {
        bytes: u32::MAX,
        limit,
    })?;
    if bytes > limit {
        return Err(ProtocolError::TooLarge { bytes, limit });
    }
    writer.write_all(&bytes.to_le_bytes())?;
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

/// Read one length-prefixed message, refusing implausible lengths before
/// allocating anything.
///
/// A peer that goes away mid-message is [`ProtocolError::Closed`] rather than
/// an I/O error: a worker exiting is ordinary, and the supervisor's restart
/// path wants to hear "gone", not "unexpected end of file".
pub fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
    limit: u32,
) -> Result<T, ProtocolError> {
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(ProtocolError::Closed)
        }
        Err(e) => return Err(ProtocolError::Io(e)),
    }
    let bytes = u32::from_le_bytes(length);
    if bytes == 0 {
        return Err(ProtocolError::Malformed("empty message".into()));
    }
    // Before the allocation, never after: this is the whole point of the
    // check, and a hostile length is exactly the case it exists for.
    if bytes > limit {
        return Err(ProtocolError::TooLarge { bytes, limit });
    }
    let mut buffer = vec![0u8; bytes as usize];
    reader.read_exact(&mut buffer).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ProtocolError::Closed
        } else {
            ProtocolError::Io(e)
        }
    })?;
    bincode::deserialize(&buffer).map_err(|e| ProtocolError::Malformed(format!("decode: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u32 = 1 << 20;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Message {
        id: u64,
        payload: String,
    }

    fn message() -> Message {
        Message {
            id: 7,
            payload: "a message".into(),
        }
    }

    #[test]
    fn a_message_round_trips() {
        let mut pipe = Vec::new();
        write_message(&mut pipe, &message(), LIMIT).unwrap();
        let read: Message = read_message(&mut pipe.as_slice(), LIMIT).unwrap();
        assert_eq!(read, message());
    }

    #[test]
    fn an_oversized_length_is_refused_before_it_is_allocated() {
        // A four-gigabyte length with no payload behind it. The reader must
        // refuse on the header alone; if it allocated first, this test would
        // take the machine down rather than fail.
        let mut pipe = u32::MAX.to_le_bytes().to_vec();
        let read = read_message::<Message>(&mut pipe.as_slice(), LIMIT);
        assert!(
            matches!(read, Err(ProtocolError::TooLarge { bytes, limit })
                if bytes == u32::MAX && limit == LIMIT),
            "a length past the ceiling is refused, and says both numbers"
        );
        pipe.clear();
    }

    #[test]
    fn writing_past_the_ceiling_is_refused_rather_than_sent() {
        let big = Message {
            id: 1,
            payload: "x".repeat(4096),
        };
        let mut pipe = Vec::new();
        let written = write_message(&mut pipe, &big, 64);
        assert!(matches!(written, Err(ProtocolError::TooLarge { .. })));
        assert!(
            pipe.is_empty(),
            "nothing may reach the pipe once the message is refused"
        );
    }

    #[test]
    fn a_truncated_message_reads_as_a_closed_peer() {
        let mut pipe = Vec::new();
        write_message(&mut pipe, &message(), LIMIT).unwrap();
        pipe.truncate(pipe.len() - 1);
        let read = read_message::<Message>(&mut pipe.as_slice(), LIMIT);
        assert!(
            matches!(read, Err(ProtocolError::Closed)),
            "a worker that dies mid-message is gone, not a broken pipe"
        );
    }

    #[test]
    fn an_empty_pipe_reads_as_a_closed_peer() {
        let read = read_message::<Message>(&mut [].as_slice(), LIMIT);
        assert!(matches!(read, Err(ProtocolError::Closed)));
    }

    #[test]
    fn a_zero_length_message_is_malformed() {
        let pipe = 0u32.to_le_bytes().to_vec();
        let read = read_message::<Message>(&mut pipe.as_slice(), LIMIT);
        assert!(matches!(read, Err(ProtocolError::Malformed(_))));
    }

    #[test]
    fn garbage_behind_a_plausible_length_does_not_panic() {
        let mut pipe = 16u32.to_le_bytes().to_vec();
        pipe.extend_from_slice(&[0xFF; 16]);
        let read = read_message::<Message>(&mut pipe.as_slice(), LIMIT);
        assert!(
            matches!(read, Err(ProtocolError::Malformed(_))),
            "a hostile worker gets an error, never a panic"
        );
    }
}

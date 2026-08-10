use std::io::{self, Read, Write};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub fn read_message<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut length_bytes = [0_u8; 4];
    reader.read_exact(&mut length_bytes)?;
    let length = u32::from_le_bytes(length_bytes) as usize;

    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native message too large",
        ));
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;

    Ok(payload)
}

pub fn write_message<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;

    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_message_rejects_oversized_length_before_allocating_payload() {
        let length = ((1024 * 1024) + 1_u32).to_le_bytes();
        let mut reader = Cursor::new(length);

        let error = read_message(&mut reader).expect_err("oversized frame should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("too large"));
    }

    #[test]
    fn framing_reads_multiple_messages_from_one_stream() {
        let mut stream = Vec::new();
        write_message(&mut stream, br#"{"type":"ping"}"#).unwrap();
        write_message(&mut stream, br#"{"type":"enqueue_download"}"#).unwrap();
        let mut reader = Cursor::new(stream);

        assert_eq!(read_message(&mut reader).unwrap(), br#"{"type":"ping"}"#);
        assert_eq!(
            read_message(&mut reader).unwrap(),
            br#"{"type":"enqueue_download"}"#
        );
        assert_eq!(
            read_message(&mut reader).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}

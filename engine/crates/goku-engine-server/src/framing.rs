//! LSP base protocol framing (`Content-Length: N\r\n\r\n` + JSON), what `vscode-jsonrpc`'s stream reader/writer speak.
use serde_json::Value;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Larger messages are refused instead of allocated: the client only sends requests, never bulk data.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

fn invalid(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

/// The next message, or `None` at a clean end of input (the client closed stdin).
pub async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return if length.is_none() { Ok(None) } else { Err(invalid("input ended inside a header")) };
        }
        if line.len() > 1024 {
            return Err(invalid("header line too long"));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue; // tolerate blank lines between messages
        }
        let (name, value) = header.split_once(':').ok_or_else(|| invalid(format!("bad header “{header}”")))?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            let n: usize = value.trim().parse().map_err(|_| invalid("bad Content-Length"))?;
            if n > MAX_MESSAGE_BYTES {
                return Err(invalid(format!("message of {n} bytes is over the {MAX_MESSAGE_BYTES} limit")));
            }
            length = Some(n);
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map(Some).map_err(|e| invalid(format!("message is not JSON: {e}")))
}

pub async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    writer.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn round_trip_and_clean_eof() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({"a": "é"})).await.unwrap();
        write_message(&mut buf, &json!([1, 2])).await.unwrap();
        let mut r = BufReader::new(&buf[..]);
        assert_eq!(read_message(&mut r).await.unwrap(), Some(json!({"a": "é"})));
        assert_eq!(read_message(&mut r).await.unwrap(), Some(json!([1, 2])));
        assert_eq!(read_message(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn extra_headers_and_case() {
        let raw = b"content-length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}";
        assert_eq!(read_message(&mut BufReader::new(&raw[..])).await.unwrap(), Some(json!({})));
    }

    #[tokio::test]
    async fn refuses_garbage() {
        for raw in [
            &b"Content-Length: x\r\n\r\n"[..],
            b"nonsense\r\n\r\n",
            b"Content-Length: 99999999999\r\n\r\n",
            b"Content-Length: 3\r\n\r\n{]}",
            b"Content-Length: 5\r\n\r\n{}",
        ] {
            assert!(read_message(&mut BufReader::new(raw)).await.is_err(), "{}", String::from_utf8_lossy(raw));
        }
    }
}

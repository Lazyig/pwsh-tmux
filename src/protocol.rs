//! Wire format between client and server: `[kind u8][len u32 LE][payload]`.

use std::io::{self, Read, Write};

// client -> server
/// argv of the command line (may hold several commands separated by ";").
pub const C_COMMAND: u8 = 1;
pub const C_INPUT: u8 = 3;
pub const C_RESIZE: u8 = 4;
/// Sent first: args [cols, rows, is_tty(0/1), cwd, term, pid, ctx_pane, control(0/1/2),
/// features (-T), KEY=VALUE...].
pub const C_IDENTIFY: u8 = 5;
/// Client finished a S_SHELL / S_SUSPEND and is back in raw mode.
pub const C_RESUMED: u8 = 6;
/// Control mode: one command line typed by the client (empty = detach).
pub const C_CONTROL_LINE: u8 = 7;

// server -> client
pub const S_OUTPUT: u8 = 10;
/// Detach/exit: payload is the message to print. Ends the client.
pub const S_EXIT: u8 = 11;
/// Command finished: code byte + text. Ends a non-attached client.
pub const S_REPLY: u8 = 12;
/// The client is now attached: enter raw mode and the alternate screen.
pub const S_ATTACHED: u8 = 13;
/// Suspend the client (Unix job control).
pub const S_SUSPEND: u8 = 14;
/// Leave raw mode, run this shell command in the client terminal, then send C_RESUMED.
pub const S_SHELL: u8 = 15;
/// Text to print on the client's stdout while it is not attached (e.g. command output before a reply).
pub const S_STDOUT: u8 = 16;
/// detach-client -E: leave and run this command instead; payload "command\0shell".
pub const S_EXEC: u8 = 17;

const MAX_FRAME: usize = 64 << 20;
pub const MAX_CLIENT_FRAME: usize = 1 << 20;
pub const MAX_STDIN_BUFFER: usize = MAX_CLIENT_FRAME / 4 * 3;
pub const STDIN_BUFFER_PREFIX: &str = "\x01stdin64:";

/// Only ASCII base64 enters argv framing; buffer bytes never become arguments.
pub fn encode_stdin_buffer(data: &[u8]) -> String {
    let mut out = String::from(STDIN_BUFFER_PREFIX);
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in data.chunks(3) {
        let value = (chunk[0] as u32) << 16 | (chunk.get(1).copied().unwrap_or(0) as u32) << 8 | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(TABLE[(value >> 18) as usize] as char);
        out.push(TABLE[((value >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((value >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(value & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn decode_stdin_buffer(argument: &str) -> Result<Vec<u8>, String> {
    let text = argument.strip_prefix(STDIN_BUFFER_PREFIX).ok_or("invalid stdin buffer encoding")?;
    if text.len() > MAX_CLIENT_FRAME || text.len() % 4 != 0 { return Err("invalid stdin buffer encoding".into()); }
    let digit = |b: u8| -> Result<u32, String> {
        Ok(match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62, b'/' => 63,
            _ => return Err("invalid stdin buffer encoding".into()),
        })
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for (index, chunk) in text.as_bytes().chunks_exact(4).enumerate() {
        let (a, b) = (digit(chunk[0])?, digit(chunk[1])?);
        let last = index + 1 == text.len() / 4;
        let c = if chunk[2] == b'=' && last && chunk[3] == b'=' { 0 } else { digit(chunk[2])? };
        let d = if chunk[3] == b'=' && last { 0 } else { digit(chunk[3])? };
        if (chunk[2] == b'=' && b & 15 != 0) || (chunk[3] == b'=' && c & 3 != 0) { return Err("invalid stdin buffer encoding".into()); }
        let value = a << 18 | b << 12 | c << 6 | d;
        out.push((value >> 16) as u8);
        if chunk[2] != b'=' { out.push((value >> 8) as u8); }
        if chunk[3] != b'=' { out.push(value as u8); }
    }
    Ok(out)
}

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    w.write_all(&frame(kind, payload))?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    read_frame_checked(r, |_, _| Ok(()))
}

/// Validate a header (including peer authorization) before allocating its body.
pub fn read_frame_checked(r: &mut impl Read, check: impl FnOnce(u8, usize) -> io::Result<()>) -> io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 5];
    r.read_exact(&mut hdr)?;
    let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    check(hdr[0], len)?;
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok((hdr[0], payload))
}

/// Each argument is NUL-terminated so empty arguments survive the round trip.
pub fn encode_args(args: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for a in args {
        out.extend_from_slice(a.as_bytes());
        out.push(0);
    }
    out
}

pub fn decode_args(p: &[u8]) -> Vec<String> {
    let mut parts: Vec<String> = String::from_utf8_lossy(p).split('\0').map(String::from).collect();
    parts.pop();
    parts
}

pub fn encode_size(cols: u16, rows: u16) -> Vec<u8> {
    let mut v = cols.to_le_bytes().to_vec();
    v.extend_from_slice(&rows.to_le_bytes());
    v
}

pub fn decode_size(p: &[u8]) -> Option<(u16, u16)> {
    if p.len() < 4 {
        return None;
    }
    Some((u16::from_le_bytes([p[0], p[1]]), u16::from_le_bytes([p[2], p[3]])))
}

pub fn encode_reply(code: u8, text: &str) -> Vec<u8> {
    let mut v = vec![code];
    v.extend_from_slice(text.as_bytes());
    v
}

pub fn decode_reply(p: &[u8]) -> (u8, String) {
    match p.split_first() {
        Some((code, rest)) => (*code, String::from_utf8_lossy(rest).into_owned()),
        None => (1, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_roundtrip() {
        let a = vec!["send-keys".to_string(), String::new(), "한글 x".to_string()];
        assert_eq!(decode_args(&encode_args(&a)), a);
        assert!(decode_args(&encode_args(&[])).is_empty());
    }

    #[test]
    fn stdin_buffers_preserve_bytes_without_changing_commands() {
        for size in [0, 1, 2, 3, 255, 256, 257] {
            let data: Vec<u8> = (0..size).map(|i| i as u8).collect();
            let argv = vec!["load-buffer".into(), encode_stdin_buffer(&data)];
            let decoded = decode_args(&encode_args(&argv));
            assert_eq!(decoded, argv);
            assert_eq!(crate::cmdparse::split_commands(&decoded).len(), 1);
            assert_eq!(decode_stdin_buffer(&decoded[1]).unwrap(), data);
        }
    }

    #[test]
    fn stdin_buffers_reject_malformed_base64() {
        for invalid in ["A", "====", "AA=A", "AA==AAAA", "AB==", "AAB=", "AA\n=", "____", "éé"] {
            assert!(decode_stdin_buffer(&format!("{STDIN_BUFFER_PREFIX}{invalid}")).is_err(), "{invalid:?}");
        }
        assert!(decode_stdin_buffer("\x01stdin:legacy").is_err());
    }

    #[test]
    fn frame_roundtrip() {
        let f = frame(S_OUTPUT, b"hello");
        let (k, p) = read_frame(&mut &f[..]).unwrap();
        assert_eq!((k, p.as_slice()), (S_OUTPUT, &b"hello"[..]));
    }

    #[test]
    fn rejected_header_does_not_read_body() {
        let mut bytes = &frame(C_IDENTIFY, b"private")[..];
        let error = read_frame_checked(&mut bytes, |_, _| Err(io::ErrorKind::PermissionDenied.into())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(bytes, b"private");
    }
}

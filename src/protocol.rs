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

const MAX_FRAME: usize = 64 << 20;

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

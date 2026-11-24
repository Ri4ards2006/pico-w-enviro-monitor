//! Minimal HTTP/1.1 client pieces: one POST per TCP connection
//! (`Connection: close`), no chunking, no redirects — all the node needs.

use core::fmt::Write;

pub type Head = heapless::String<512>;

/// Build the request head. `authorization` is the full header value
/// (`Basic …` / `Bearer …`, precomputed by build.rs); empty = no header.
pub fn request_head(
    host: &str,
    path: &str,
    content_type: &str,
    authorization: &str,
    body_len: usize,
) -> Result<Head, core::fmt::Error> {
    let mut h = Head::new();
    write!(
        h,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pico-w-enviro-monitor/0.1\r\n\
         Content-Type: {content_type}\r\nContent-Length: {body_len}\r\nConnection: close\r\n"
    )?;
    if !authorization.is_empty() {
        write!(h, "Authorization: {authorization}\r\n")?;
    }
    h.write_str("\r\n")?;
    Ok(h)
}

/// Parse the status code from the first bytes of a response
/// (`HTTP/1.x NNN ...`). Returns `None` until the code is complete.
pub fn parse_status(resp: &[u8]) -> Option<u16> {
    let rest = resp.strip_prefix(b"HTTP/1.")?;
    let code = rest.get(2..5)?; // skip minor version digit + space
    if rest.get(1) != Some(&b' ') || !code.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(code.iter().fold(0u16, |a, d| a * 10 + (d - b'0') as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_without_auth() {
        let h = request_head("sink.example", "/telegraf", "application/json", "", 17).unwrap();
        assert_eq!(
            h.as_str(),
            "POST /telegraf HTTP/1.1\r\nHost: sink.example\r\nUser-Agent: pico-w-enviro-monitor/0.1\r\n\
             Content-Type: application/json\r\nContent-Length: 17\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn head_with_authorization() {
        let h = request_head("h", "/", "application/json", "Basic dTpw", 0).unwrap();
        assert!(h.ends_with("Connection: close\r\nAuthorization: Basic dTpw\r\n\r\n"));
    }

    #[test]
    fn status_parsing() {
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(parse_status(b"HTTP/1.0 200 OK"), Some(200));
        assert_eq!(parse_status(b"HTTP/1.1 50"), None); // incomplete
        assert_eq!(parse_status(b"garbage"), None);
    }
}

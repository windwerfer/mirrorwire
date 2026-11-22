//! RTSP 1.0 (RFC 2326) message framing: parse and encode, no I/O.
//!
//! Input comes from the network, so everything is bounded (`MAX_HEAD`, `MAX_BODY`) and a bad
//! message is an `Err`, never a panic (AGENTS.md 1.9).

use std::fmt;

/// Longest request/status line plus headers we accept.
pub const MAX_HEAD: usize = 8 * 1024;
/// Longest body we accept (capability lists are a few hundred bytes).
pub const MAX_BODY: usize = 32 * 1024;
const MAX_HEADERS: usize = 64;

/// A framing or syntax error. The message is shown in the GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(msg: &str) -> Result<T, ParseError> {
    Err(ParseError(msg.to_string()))
}

/// Header list; names compare case-insensitively, order is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn push(&mut self, name: &str, value: impl Into<String>) {
        self.0.push((name.to_string(), value.into()));
    }

    pub fn cseq(&self) -> Option<u32> {
        self.get("CSeq")?.trim().parse().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub uri: String,
    pub headers: Headers,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
}

impl Message {
    pub fn headers(&self) -> &Headers {
        match self {
            Message::Request(r) => &r.headers,
            Message::Response(r) => &r.headers,
        }
    }
}

fn encode_common(start: &str, headers: &Headers, body: &str) -> Vec<u8> {
    let mut out = String::with_capacity(start.len() + body.len() + 64);
    out.push_str(start);
    out.push_str("\r\n");
    for (n, v) in &headers.0 {
        if n.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        out.push_str(n);
        out.push_str(": ");
        out.push_str(v);
        out.push_str("\r\n");
    }
    if !body.is_empty() {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out.into_bytes()
}

impl Request {
    pub fn new(method: &str, uri: &str, cseq: u32) -> Self {
        let mut headers = Headers::default();
        headers.push("CSeq", cseq.to_string());
        Request {
            method: method.to_string(),
            uri: uri.to_string(),
            headers,
            body: String::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push(name, value);
        self
    }

    /// Sets the body and `Content-Type: text/parameters`.
    pub fn with_params_body(mut self, body: String) -> Self {
        self.headers.push("Content-Type", "text/parameters");
        self.body = body;
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        encode_common(
            &format!("{} {} RTSP/1.0", self.method, self.uri),
            &self.headers,
            &self.body,
        )
    }
}

impl Response {
    pub fn new(status: u16, cseq: u32) -> Self {
        let mut headers = Headers::default();
        headers.push("CSeq", cseq.to_string());
        Response {
            status,
            reason: reason(status).to_string(),
            headers,
            body: String::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push(name, value);
        self
    }

    pub fn with_params_body(mut self, body: String) -> Self {
        self.headers.push("Content-Type", "text/parameters");
        self.body = body;
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        encode_common(
            &format!("RTSP/1.0 {} {}", self.status, self.reason),
            &self.headers,
            &self.body,
        )
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        455 => "Method Not Valid in This State",
        551 => "Option not supported",
        _ => "Error",
    }
}

/// Finds the end of the header block: index just after the blank line.
fn head_end(buf: &[u8]) -> Option<usize> {
    buf.iter().enumerate().find_map(|(i, &c)| {
        if c != b'\n' {
            return None;
        }
        let rest = buf.get(i + 1..)?;
        if rest.first() == Some(&b'\n') {
            Some(i + 2)
        } else if rest.starts_with(b"\r\n") {
            Some(i + 3)
        } else {
            None
        }
    })
}

/// Tries to parse one message from the front of `buf`.
///
/// `Ok(None)` means more bytes are needed; `Ok(Some((msg, used)))` consumed `used` bytes.
pub fn parse_message(buf: &[u8]) -> Result<Option<(Message, usize)>, ParseError> {
    let Some(end) = head_end(buf) else {
        if buf.len() > MAX_HEAD {
            return err("RTSP header too long");
        }
        return Ok(None);
    };
    if end > MAX_HEAD {
        return err("RTSP header too long");
    }
    let head = buf
        .get(..end)
        .and_then(|h| std::str::from_utf8(h).ok())
        .ok_or_else(|| ParseError("RTSP header is not text".into()))?;
    let mut lines = head.lines();
    let start = lines.next().unwrap_or("").trim();
    if start.is_empty() {
        return err("empty RTSP start line");
    }
    let mut headers = Headers::default();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((n, v)) = line.split_once(':') else {
            return err("RTSP header line without colon");
        };
        if headers.0.len() >= MAX_HEADERS {
            return err("too many RTSP headers");
        }
        headers.push(n.trim(), v.trim());
    }
    let len = match headers.get("Content-Length") {
        None => 0,
        Some(v) => v
            .trim()
            .parse::<usize>()
            .map_err(|_| ParseError("bad Content-Length".into()))?,
    };
    if len > MAX_BODY {
        return err("RTSP body too long");
    }
    let total = end
        .checked_add(len)
        .ok_or_else(|| ParseError("length overflow".into()))?;
    if buf.len() < total {
        return Ok(None);
    }
    let body = String::from_utf8_lossy(buf.get(end..total).unwrap_or_default()).into_owned();

    let msg = if let Some(rest) = start.strip_prefix("RTSP/") {
        // "RTSP/1.0 200 OK"
        let mut it = rest.splitn(3, ' ');
        let _ver = it.next();
        let status = it
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| ParseError("bad RTSP status line".into()))?;
        let reason = it.next().unwrap_or("").to_string();
        Message::Response(Response {
            status,
            reason,
            headers,
            body,
        })
    } else {
        let mut it = start.split_whitespace();
        let (Some(method), Some(uri), Some(ver)) = (it.next(), it.next(), it.next()) else {
            return err("bad RTSP request line");
        };
        if !ver.starts_with("RTSP/") {
            return err("not an RTSP request");
        }
        Message::Request(Request {
            method: method.to_string(),
            uri: uri.to_string(),
            headers,
            body,
        })
    };
    Ok(Some((msg, total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_body_and_waits_for_more() {
        let raw = b"SET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 4\r\nContent-Type: text/parameters\r\nContent-Length: 20\r\n\r\nwfd_trigger_method: SETUP\r\n";
        assert_eq!(parse_message(&raw[..30]), Ok(None));
        assert_eq!(parse_message(&raw[..raw.len() - 8]), Ok(None));
        let (m, used) = parse_message(raw).unwrap().unwrap();
        // Content-Length says 20, so the trailing bytes stay in the buffer.
        assert_eq!(used, raw.len() - 7);
        let Message::Request(r) = m else { panic!() };
        assert_eq!(r.method, "SET_PARAMETER");
        assert_eq!(r.headers.cseq(), Some(4));
        assert_eq!(r.body.len(), 20);
    }

    #[test]
    fn parses_response_and_roundtrips() {
        let r = Response::new(200, 7).with_header("Public", "org.wfa.wfd1.0, GET_PARAMETER");
        let (m, used) = parse_message(&r.encode()).unwrap().unwrap();
        assert_eq!(used, r.encode().len());
        assert_eq!(m, Message::Response(r));
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let raw = b"OPTIONS * RTSP/1.0\r\ncseq: 1\r\nREQUIRE: org.wfa.wfd1.0\r\n\r\n";
        let (m, _) = parse_message(raw).unwrap().unwrap();
        assert_eq!(m.headers().cseq(), Some(1));
        assert_eq!(m.headers().get("require"), Some("org.wfa.wfd1.0"));
    }

    #[test]
    fn accepts_bare_lf() {
        let raw = b"OPTIONS * RTSP/1.0\nCSeq: 1\n\n";
        assert!(parse_message(raw).unwrap().is_some());
    }

    #[test]
    fn rejects_hostile_input_without_panicking() {
        assert!(parse_message(&vec![b'a'; MAX_HEAD + 1]).is_err());
        assert!(parse_message(b"OPTIONS * RTSP/1.0\r\nContent-Length: 99999999\r\n\r\n").is_err());
        assert!(parse_message(b"OPTIONS * RTSP/1.0\r\nContent-Length: -1\r\n\r\n").is_err());
        assert!(parse_message(b"OPTIONS * RTSP/1.0\r\nnocolon\r\n\r\n").is_err());
        assert!(parse_message(b"GET /\r\n\r\n").is_err());
        assert!(parse_message(b"RTSP/1.0 abc\r\n\r\n").is_err());
        assert!(parse_message(b"\xff\xfe\r\n\r\n").is_err());
        assert!(parse_message(b"\r\n\r\n").is_err());
    }
}

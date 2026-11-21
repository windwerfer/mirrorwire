use std::time::{Duration, Instant};

use crate::message::{parse_message, Message, ParseError, Request, Response, MAX_BODY, MAX_HEAD};
use crate::params::{
    audio_accepts, encode_audio_codecs, parse_audio_codecs, parse_params, parse_presentation_url,
    single_cea, AudioCodec, ClientRtpPorts, H264Codec, Resolution, VideoFormats,
};

const FEATURE: &str = "org.wfa.wfd1.0";
const PUBLIC: &str = "org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER";
/// RTSP default session timeout when the source names none (RFC 2326).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PENDING: usize = 16;
/// At most one IDR request (M13) per this interval: a loss burst gives many `need_keyframe`
/// episodes, and the source needs only one request per keyframe (hardware run 4: requests
/// 3 s apart, a second one while the link was already gone).
pub const MIN_IDR_INTERVAL: Duration = Duration::from_secs(1);

/// Where the sink is in the M1-M7 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// Waiting for the source's M1 (`OPTIONS`).
    #[default]
    AwaitOptions,
    /// M1 answered, M2 sent; waiting for M3 (`GET_PARAMETER`).
    AwaitCapabilityQuery,
    /// M3 answered; waiting for M4 (`SET_PARAMETER` with the chosen formats).
    AwaitParameters,
    /// M4 accepted; waiting for M5 (`wfd_trigger_method: SETUP`).
    AwaitTrigger,
    /// M6 (`SETUP`) sent; waiting for the reply.
    AwaitSetupReply,
    /// M7 (`PLAY`) sent; waiting for the reply.
    AwaitPlayReply,
    /// Media flows; keepalives (M16) answered.
    Playing,
    /// TEARDOWN seen or sent, or a fatal error.
    Closed,
}

/// What the source picked in M4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    pub video: H264Codec,
    /// Set when the pick is exactly one CEA entry.
    pub resolution: Option<Resolution>,
    pub audio: Option<Vec<AudioCodec>>,
    pub presentation_url: String,
    pub rtp_port: u16,
}

/// What the I/O layer must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write these bytes to the RTSP TCP connection.
    Send(Vec<u8>),
    /// M4 accepted (also after a mid-stream re-negotiation): prepare the decoder.
    Negotiated(Negotiated),
    /// Start receiving RTP on the announced port.
    StartMedia {
        rtp_port: u16,
    },
    StopMedia,
    /// Close the connection; the reason is shown in the GUI (AGENTS.md 1.8).
    Close {
        reason: String,
    },
}

/// What the sink offers in M3.
#[derive(Debug, Clone)]
pub struct SinkConfig {
    pub video: VideoFormats,
    /// `None` advertises `none` (no audio).
    pub audio: Option<Vec<AudioCodec>>,
    pub rtp_port: u16,
}

impl Default for SinkConfig {
    fn default() -> Self {
        let entry = |profile| H264Codec {
            profile,
            level: 0x04,
            cea: 0x1E1, // 640x480p60, 720p30, 720p60, 1080p30, 1080p60
            vesa: 0,
            hh: 0,
            latency: 0,
            min_slice_size: 0,
            slice_enc_params: 0,
            frame_rate_control: 0,
            max_hres: None,
            max_vres: None,
        };
        SinkConfig {
            video: VideoFormats {
                native: 5 << 3, // CEA index 5 = 1280x720p30
                preferred_display_mode: 0,
                codecs: vec![entry(0x01), entry(0x02)],
            },
            audio: None,
            rtp_port: 19000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Options,
    Setup,
    Play,
    Teardown,
    Idr,
}

/// Sink-side session.
#[derive(Debug)]
pub struct SinkSession {
    cfg: SinkConfig,
    state: State,
    buf: Vec<u8>,
    next_cseq: u32,
    pending: Vec<(u32, Pending)>,
    url: Option<String>,
    session_id: Option<String>,
    timeout: Duration,
    last_rx: Option<Instant>,
    media_started: bool,
    last_idr: Option<Instant>,
    /// IDR requests not sent because of [`MIN_IDR_INTERVAL`].
    pub idr_suppressed: u64,
}

impl Default for SinkSession {
    fn default() -> Self {
        Self::new(SinkConfig::default())
    }
}

fn status(code: u16, cseq: u32) -> Action {
    Action::Send(Response::new(code, cseq).encode())
}

impl SinkSession {
    pub fn new(cfg: SinkConfig) -> Self {
        SinkSession {
            cfg,
            state: State::AwaitOptions,
            buf: Vec::new(),
            next_cseq: 1,
            pending: Vec::new(),
            url: None,
            session_id: None,
            timeout: DEFAULT_TIMEOUT,
            last_rx: None,
            media_started: false,
            last_idr: None,
            idr_suppressed: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Feed bytes read from the TCP connection.
    pub fn handle_input(&mut self, data: &[u8], now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if self.state == State::Closed {
            return out;
        }
        self.last_rx = Some(now);
        if self.buf.len() + data.len() > MAX_HEAD + MAX_BODY {
            self.fail(&mut out, "RTSP input buffer overflow");
            return out;
        }
        self.buf.extend_from_slice(data);
        loop {
            match parse_message(&self.buf) {
                Ok(None) => break,
                Ok(Some((msg, used))) => {
                    self.buf.drain(..used);
                    self.dispatch(msg, &mut out);
                    if self.state == State::Closed {
                        break;
                    }
                }
                Err(ParseError(e)) => {
                    self.fail(&mut out, &format!("bad RTSP message from source: {e}"));
                    break;
                }
            }
        }
        out
    }

    /// Call on a timer (about once a second). Detects a silent source.
    pub fn poll(&mut self, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if self.state != State::Closed
            && self
                .last_rx
                .is_some_and(|t| now.saturating_duration_since(t) > self.timeout)
        {
            self.fail(
                &mut out,
                "the source stopped talking (RTSP keepalive timeout)",
            );
        }
        out
    }

    /// Ask the source for a keyframe (M13) after loss.
    /// M13 `wfd_idr_request`, rate-limited to one per [`MIN_IDR_INTERVAL`]; an empty result
    /// means the request was suppressed (counted in `idr_suppressed`) or not playing.
    pub fn request_idr(&mut self, now: Instant) -> Vec<Action> {
        if self.state != State::Playing {
            return Vec::new();
        }
        if self
            .last_idr
            .is_some_and(|t| now.saturating_duration_since(t) < MIN_IDR_INTERVAL)
        {
            self.idr_suppressed += 1;
            return Vec::new();
        }
        self.last_idr = Some(now);
        let req = self.request("SET_PARAMETER", Pending::Idr, |r| {
            r.with_params_body("wfd_idr_request\r\n".into())
        });
        vec![Action::Send(req)]
    }

    /// Tear down locally (M8): tells the source, stops media, closes.
    pub fn teardown(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        if self.state == State::Closed {
            return out;
        }
        if let (Some(url), Some(id)) = (self.url.clone(), self.session_id.clone()) {
            let bytes = self.request_to("TEARDOWN", &url, Pending::Teardown, |r| {
                r.with_header("Session", id.clone())
            });
            out.push(Action::Send(bytes));
        }
        self.close(&mut out, "stopped by the user");
        out
    }

    fn request(
        &mut self,
        method: &str,
        kind: Pending,
        f: impl FnOnce(Request) -> Request,
    ) -> Vec<u8> {
        self.request_to(method, "rtsp://localhost/wfd1.0", kind, f)
    }

    fn request_to(
        &mut self,
        method: &str,
        uri: &str,
        kind: Pending,
        f: impl FnOnce(Request) -> Request,
    ) -> Vec<u8> {
        let cseq = self.next_cseq;
        self.next_cseq = self.next_cseq.wrapping_add(1);
        if self.pending.len() >= MAX_PENDING {
            self.pending.remove(0);
        }
        self.pending.push((cseq, kind));
        f(Request::new(method, uri, cseq)).encode()
    }

    fn fail(&mut self, out: &mut Vec<Action>, reason: &str) {
        self.close(out, reason);
    }

    fn close(&mut self, out: &mut Vec<Action>, reason: &str) {
        if self.media_started {
            out.push(Action::StopMedia);
            self.media_started = false;
        }
        self.state = State::Closed;
        out.push(Action::Close {
            reason: reason.to_string(),
        });
    }

    fn dispatch(&mut self, msg: Message, out: &mut Vec<Action>) {
        match msg {
            Message::Request(r) => self.on_request(r, out),
            Message::Response(r) => self.on_response(r, out),
        }
    }

    fn on_request(&mut self, req: Request, out: &mut Vec<Action>) {
        let Some(cseq) = req.headers.cseq() else {
            self.fail(out, "RTSP request from source without CSeq");
            return;
        };
        match req.method.as_str() {
            "OPTIONS" => self.on_options(&req, cseq, out),
            "GET_PARAMETER" => self.on_get_parameter(&req, cseq, out),
            "SET_PARAMETER" => self.on_set_parameter(&req, cseq, out),
            "TEARDOWN" => {
                out.push(status(200, cseq));
                self.close(out, "the source ended the session");
            }
            _ => out.push(status(405, cseq)),
        }
    }

    fn on_options(&mut self, req: &Request, cseq: u32, out: &mut Vec<Action>) {
        if let Some(req_feat) = req.headers.get("Require") {
            if !req_feat
                .split(',')
                .any(|f| f.trim().eq_ignore_ascii_case(FEATURE))
            {
                out.push(status(551, cseq));
                self.fail(out, "the source requires an RTSP feature this sink lacks");
                return;
            }
        }
        out.push(Action::Send(
            Response::new(200, cseq)
                .with_header("Public", PUBLIC)
                .encode(),
        ));
        if self.state == State::AwaitOptions {
            // M2: our own OPTIONS.
            let m2 = self.request_to("OPTIONS", "*", Pending::Options, |r| {
                r.with_header("Require", FEATURE)
            });
            out.push(Action::Send(m2));
            self.state = State::AwaitCapabilityQuery;
        }
    }

    fn on_get_parameter(&mut self, req: &Request, cseq: u32, out: &mut Vec<Action>) {
        if self.state == State::AwaitOptions {
            out.push(status(455, cseq));
            return;
        }
        if req.body.trim().is_empty() {
            // M16 keepalive.
            out.push(status(200, cseq));
            return;
        }
        let mut body = String::new();
        for (name, _) in parse_params(&req.body) {
            let value = match name {
                "wfd_video_formats" => self.cfg.video.encode(),
                "wfd_audio_codecs" => encode_audio_codecs(&self.cfg.audio),
                "wfd_client_rtp_ports" => ClientRtpPorts {
                    profile: "RTP/AVP/UDP;unicast".into(),
                    port0: self.cfg.rtp_port,
                    port1: 0,
                }
                .encode(),
                "wfd_content_protection"
                | "wfd_display_edid"
                | "wfd_coupled_sink"
                | "wfd_uibc_capability"
                | "wfd_standby_resume_capability"
                | "wfd_connector_type" => "none".to_string(),
                // Unknown names are left out of the reply.
                _ => continue,
            };
            body.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push(Action::Send(
            Response::new(200, cseq).with_params_body(body).encode(),
        ));
        if self.state == State::AwaitCapabilityQuery {
            self.state = State::AwaitParameters;
        }
    }

    fn on_set_parameter(&mut self, req: &Request, cseq: u32, out: &mut Vec<Action>) {
        if self.state == State::AwaitOptions {
            out.push(status(455, cseq));
            return;
        }
        let params = parse_params(&req.body);
        let get = |n: &str| find_param(&params, n);

        if let Some(trigger) = get("wfd_trigger_method") {
            self.on_trigger(trigger, cseq, out);
            return;
        }
        if get("wfd_presentation_URL").is_some() || get("wfd_video_formats").is_some() {
            if !matches!(self.state, State::AwaitParameters | State::Playing) {
                out.push(status(455, cseq));
                return;
            }
            match self.check_m4(&params) {
                Ok(n) => {
                    out.push(status(200, cseq));
                    self.url = Some(n.presentation_url.clone());
                    out.push(Action::Negotiated(n));
                    if self.state == State::AwaitParameters {
                        self.state = State::AwaitTrigger;
                    }
                }
                Err(e) => {
                    out.push(status(406, cseq));
                    self.fail(out, &format!("source chose an unsupported format: {e}"));
                }
            }
            return;
        }
        // Other parameters (standby, uibc settings, ...): accept and ignore.
        out.push(status(200, cseq));
    }

    fn check_m4(&self, params: &[(&str, &str)]) -> Result<Negotiated, ParseError> {
        let get = |n: &str| find_param(params, n);
        let bad = |m: &str| Err(ParseError(m.to_string()));
        let Some(v) = get("wfd_video_formats") else {
            return bad("no wfd_video_formats");
        };
        let Some(chosen) = VideoFormats::parse(v)? else {
            return bad("no video");
        };
        if !self.cfg.video.accepts(&chosen) {
            return bad("video format was not offered");
        }
        let audio = match get("wfd_audio_codecs") {
            Some(a) => parse_audio_codecs(a)?,
            None => None,
        };
        if !audio_accepts(&self.cfg.audio, &audio) {
            return bad("audio format was not offered");
        }
        let Some(url) = get("wfd_presentation_URL") else {
            return bad("no wfd_presentation_URL");
        };
        let presentation_url = parse_presentation_url(url)?;
        let rtp_port = match get("wfd_client_rtp_ports") {
            Some(p) => {
                let p = ClientRtpPorts::parse(p)?;
                if p.port0 != self.cfg.rtp_port {
                    return bad("source changed the RTP port");
                }
                p.port0
            }
            None => self.cfg.rtp_port,
        };
        let Some(video) = chosen.codecs.into_iter().next() else {
            return bad("no codec");
        };
        let resolution = single_cea(video.cea);
        Ok(Negotiated {
            video,
            resolution,
            audio,
            presentation_url,
            rtp_port,
        })
    }

    fn on_trigger(&mut self, trigger: &str, cseq: u32, out: &mut Vec<Action>) {
        match trigger.trim().to_ascii_uppercase().as_str() {
            "SETUP" if self.state == State::AwaitTrigger => {
                out.push(status(200, cseq));
                let Some(url) = self.url.clone() else {
                    self.fail(out, "SETUP trigger before a presentation URL");
                    return;
                };
                let port = self.cfg.rtp_port;
                let m6 = self.request_to("SETUP", &url, Pending::Setup, |r| {
                    r.with_header(
                        "Transport",
                        format!("RTP/AVP/UDP;unicast;client_port={port}"),
                    )
                });
                out.push(Action::Send(m6));
                self.state = State::AwaitSetupReply;
            }
            "SETUP" => out.push(status(455, cseq)),
            "TEARDOWN" => {
                out.push(status(200, cseq));
                out.extend(self.teardown());
            }
            // PLAY / PAUSE triggers: we are already playing or will be.
            _ => out.push(status(200, cseq)),
        }
    }

    fn on_response(&mut self, resp: Response, out: &mut Vec<Action>) {
        let Some(cseq) = resp.headers.cseq() else {
            return;
        };
        let Some(i) = self.pending.iter().position(|(c, _)| *c == cseq) else {
            return;
        };
        let (_, kind) = self.pending.remove(i);
        let ok = resp.status == 200;
        match kind {
            Pending::Options if !ok => self.fail(out, "the source rejected our OPTIONS (M2)"),
            Pending::Setup => {
                if !ok {
                    self.fail(
                        out,
                        &format!("the source rejected SETUP (M6): {}", resp.status),
                    );
                    return;
                }
                let Some(sess) = resp.headers.get("Session") else {
                    self.fail(out, "SETUP reply without a Session header");
                    return;
                };
                let mut it = sess.split(';');
                let id = it.next().unwrap_or("").trim();
                if id.is_empty() || id.len() > 64 || id.chars().any(char::is_control) {
                    self.fail(out, "SETUP reply with a bad Session id");
                    return;
                }
                for p in it {
                    if let Some(t) = p.trim().strip_prefix("timeout=") {
                        if let Ok(s) = t.trim().parse::<u64>() {
                            self.timeout = Duration::from_secs(s.clamp(5, 3600));
                        }
                    }
                }
                self.session_id = Some(id.to_string());
                let Some(url) = self.url.clone() else { return };
                let id = id.to_string();
                let m7 = self.request_to("PLAY", &url, Pending::Play, |r| {
                    r.with_header("Session", id)
                });
                out.push(Action::Send(m7));
                self.state = State::AwaitPlayReply;
            }
            Pending::Play => {
                if !ok {
                    self.fail(
                        out,
                        &format!("the source rejected PLAY (M7): {}", resp.status),
                    );
                    return;
                }
                self.media_started = true;
                self.state = State::Playing;
                out.push(Action::StartMedia {
                    rtp_port: self.cfg.rtp_port,
                });
            }
            Pending::Teardown | Pending::Idr | Pending::Options => {}
        }
    }
}

fn find_param<'a>(params: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Response};

    const M1: &str = "OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n";
    const M3_BODY: &str = "wfd_client_rtp_ports\r\nwfd_audio_codecs\r\nwfd_video_formats\r\nwfd_content_protection\r\nwfd_foo_bar\r\n";
    const M4_BODY: &str = "wfd_video_formats: 00 00 01 04 00000080 00000000 00000000 00 0000 0000 00 none none\r\nwfd_audio_codecs: none\r\nwfd_presentation_URL: rtsp://192.168.49.1/wfd1.0/streamid=0 none\r\nwfd_client_rtp_ports: RTP/AVP/UDP;unicast 19000 0 mode=play\r\n";

    fn req(method: &str, uri: &str, cseq: u32, body: &str) -> Vec<u8> {
        Request::new(method, uri, cseq)
            .with_header("Require", FEATURE)
            .with_params_body(body.to_string())
            .encode()
    }

    fn sent(actions: &[Action]) -> Vec<Message> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(b) => parse_message(b).unwrap().map(|(m, _)| m),
                _ => None,
            })
            .collect()
    }

    fn resp(m: &Message) -> &Response {
        match m {
            Message::Response(r) => r,
            _ => panic!("expected response, got {m:?}"),
        }
    }

    fn request(m: &Message) -> &Request {
        match m {
            Message::Request(r) => r,
            _ => panic!("expected request, got {m:?}"),
        }
    }

    /// Runs M1..M7 and returns the session in `Playing`.
    fn playing(t: Instant) -> SinkSession {
        let mut s = SinkSession::default();
        let a = s.handle_input(M1.as_bytes(), t);
        let m = sent(&a);
        assert_eq!(m.len(), 2);
        assert_eq!(resp(&m[0]).status, 200);
        assert_eq!(resp(&m[0]).headers.get("Public"), Some(PUBLIC));
        assert_eq!(request(&m[1]).method, "OPTIONS"); // M2
        assert_eq!(s.state(), State::AwaitCapabilityQuery);
        let m2_cseq = request(&m[1]).headers.cseq().unwrap();
        s.handle_input(
            &Response::new(200, m2_cseq)
                .with_header("Public", PUBLIC)
                .encode(),
            t,
        );

        let a = s.handle_input(
            &req("GET_PARAMETER", "rtsp://localhost/wfd1.0", 2, M3_BODY),
            t,
        );
        let m = sent(&a);
        let body = &resp(&m[0]).body;
        assert!(body.contains("wfd_content_protection: none"));
        assert!(body.contains("wfd_audio_codecs: none"));
        assert!(body.contains("wfd_client_rtp_ports: RTP/AVP/UDP;unicast 19000 0 mode=play"));
        assert!(!body.contains("wfd_foo_bar"));
        assert_eq!(s.state(), State::AwaitParameters);

        let a = s.handle_input(
            &req("SET_PARAMETER", "rtsp://localhost/wfd1.0", 3, M4_BODY),
            t,
        );
        assert!(matches!(&a[1], Action::Negotiated(n) if n.resolution.unwrap().height == 1080));
        assert_eq!(resp(&sent(&a)[0]).status, 200);
        assert_eq!(s.state(), State::AwaitTrigger);

        let a = s.handle_input(
            &req(
                "SET_PARAMETER",
                "rtsp://localhost/wfd1.0",
                4,
                "wfd_trigger_method: SETUP\r\n",
            ),
            t,
        );
        let m = sent(&a);
        assert_eq!(resp(&m[0]).status, 200);
        let setup = request(&m[1]);
        assert_eq!(setup.method, "SETUP"); // M6
        assert_eq!(setup.uri, "rtsp://192.168.49.1/wfd1.0/streamid=0");
        assert_eq!(
            setup.headers.get("Transport"),
            Some("RTP/AVP/UDP;unicast;client_port=19000")
        );
        assert_eq!(s.state(), State::AwaitSetupReply);

        let ok = Response::new(200, setup.headers.cseq().unwrap())
            .with_header("Session", "448846178;timeout=30")
            .with_header(
                "Transport",
                "RTP/AVP/UDP;unicast;client_port=19000;server_port=7016",
            );
        let a = s.handle_input(&ok.encode(), t);
        let m = sent(&a);
        let play = request(&m[0]);
        assert_eq!(play.method, "PLAY"); // M7
        assert_eq!(play.headers.get("Session"), Some("448846178"));
        assert_eq!(s.state(), State::AwaitPlayReply);

        let a = s.handle_input(
            &Response::new(200, play.headers.cseq().unwrap()).encode(),
            t,
        );
        assert_eq!(a, vec![Action::StartMedia { rtp_port: 19000 }]);
        assert_eq!(s.state(), State::Playing);
        s
    }

    #[test]
    fn full_m1_to_m7() {
        playing(Instant::now());
    }

    #[test]
    fn input_split_into_single_bytes() {
        let t = Instant::now();
        let mut s = SinkSession::default();
        let mut sends = 0;
        for b in M1.bytes() {
            sends += sent(&s.handle_input(&[b], t)).len();
        }
        assert_eq!(sends, 2);
    }

    #[test]
    fn keepalive_idr_and_teardown() {
        let t = Instant::now();
        let mut s = playing(t);
        let a = s.handle_input(b"GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 9\r\nSession: 448846178\r\n\r\n", t);
        assert_eq!(resp(&sent(&a)[0]).status, 200);
        assert_eq!(resp(&sent(&a)[0]).headers.cseq(), Some(9));

        let a = s.request_idr(t);
        let m = sent(&a);
        assert_eq!(request(&m[0]).body, "wfd_idr_request\r\n");

        let a = s.teardown();
        let m = sent(&a);
        assert_eq!(request(&m[0]).method, "TEARDOWN");
        assert_eq!(request(&m[0]).headers.get("Session"), Some("448846178"));
        assert_eq!(a[1], Action::StopMedia);
        assert!(matches!(a[2], Action::Close { .. }));
        assert_eq!(s.state(), State::Closed);
        assert!(s.handle_input(M1.as_bytes(), t).is_empty());
    }

    #[test]
    fn idr_requests_are_rate_limited() {
        let t = Instant::now();
        let mut s = playing(t);
        let ms = |n| t + Duration::from_millis(n);
        assert_eq!(sent(&s.request_idr(ms(0))).len(), 1);
        assert!(s.request_idr(ms(10)).is_empty());
        assert!(s.request_idr(ms(999)).is_empty());
        assert_eq!(s.idr_suppressed, 2);
        let m = sent(&s.request_idr(ms(1000)));
        assert_eq!(request(&m[0]).body, "wfd_idr_request\r\n");
        // Not playing: nothing, and not counted as suppressed.
        let mut idle = SinkSession::default();
        assert!(idle.request_idr(t).is_empty());
        assert_eq!(idle.idr_suppressed, 0);
    }

    #[test]
    fn source_teardown_stops_media() {
        let t = Instant::now();
        let mut s = playing(t);
        let a = s.handle_input(b"TEARDOWN rtsp://x/wfd1.0 RTSP/1.0\r\nCSeq: 11\r\n\r\n", t);
        assert_eq!(resp(&sent(&a)[0]).status, 200);
        assert!(a.contains(&Action::StopMedia));
        assert_eq!(s.state(), State::Closed);
    }

    #[test]
    fn keepalive_timeout() {
        let t = Instant::now();
        let mut s = playing(t); // timeout=30
        assert!(s.poll(t + Duration::from_secs(29)).is_empty());
        let a = s.poll(t + Duration::from_secs(31));
        assert_eq!(a[0], Action::StopMedia);
        assert!(matches!(&a[1], Action::Close { reason } if reason.contains("keepalive")));
    }

    #[test]
    fn rejects_unoffered_format() {
        let t = Instant::now();
        let mut s = SinkSession::default();
        s.handle_input(M1.as_bytes(), t);
        s.handle_input(&req("GET_PARAMETER", "x", 2, M3_BODY), t);
        // 1080i60 (bit 9) was not offered.
        let body = M4_BODY.replace("00000080", "00000200");
        let a = s.handle_input(&req("SET_PARAMETER", "x", 3, &body), t);
        assert_eq!(resp(&sent(&a)[0]).status, 406);
        assert!(matches!(a.last(), Some(Action::Close { .. })));
    }

    #[test]
    fn rejects_unknown_feature_tag_and_bad_state() {
        let t = Instant::now();
        let mut s = SinkSession::default();
        let a = s.handle_input(
            b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.example.other\r\n\r\n",
            t,
        );
        assert_eq!(resp(&sent(&a)[0]).status, 551);
        assert_eq!(s.state(), State::Closed);

        let mut s = SinkSession::default();
        let a = s.handle_input(&req("GET_PARAMETER", "x", 1, M3_BODY), t);
        assert_eq!(resp(&sent(&a)[0]).status, 455); // M3 before M1
        assert_eq!(s.state(), State::AwaitOptions);
    }

    #[test]
    fn setup_failure_and_garbage_close_with_reason() {
        let t = Instant::now();
        let mut s = playing(t);
        let a = s.handle_input(b"\x00\x01garbage\r\n\r\n", t);
        assert!(matches!(a.last(), Some(Action::Close { reason }) if reason.contains("bad RTSP")));

        // A failing SETUP reply.
        let mut s = SinkSession::default();
        s.handle_input(M1.as_bytes(), t);
        s.handle_input(&req("GET_PARAMETER", "x", 2, M3_BODY), t);
        s.handle_input(&req("SET_PARAMETER", "x", 3, M4_BODY), t);
        let a = s.handle_input(
            &req("SET_PARAMETER", "x", 4, "wfd_trigger_method: SETUP\r\n"),
            t,
        );
        let cseq = request(&sent(&a)[1]).headers.cseq().unwrap();
        let a = s.handle_input(&Response::new(400, cseq).encode(), t);
        assert!(matches!(a.last(), Some(Action::Close { reason }) if reason.contains("SETUP")));
    }

    #[test]
    fn oversized_input_is_refused() {
        let mut s = SinkSession::default();
        let a = s.handle_input(&vec![b'a'; MAX_HEAD + MAX_BODY + 1], Instant::now());
        assert!(matches!(a.last(), Some(Action::Close { .. })));
    }

    #[test]
    fn renegotiation_while_playing() {
        let t = Instant::now();
        let mut s = playing(t);
        let body = M4_BODY.replace("00000080", "00000020"); // 720p30
        let a = s.handle_input(&req("SET_PARAMETER", "x", 20, &body), t);
        assert!(matches!(&a[1], Action::Negotiated(n) if n.resolution.unwrap().height == 720));
        assert_eq!(s.state(), State::Playing);
    }
}

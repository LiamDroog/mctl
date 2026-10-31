//! Minimal Lightstreamer TLCP 2.5.0 protocol layer: request encoding and notification parsing.
//!
//! Pure functions, no I/O, so it can move into `mctl-sources` unchanged. Only the subset ISSLIVE
//! needs is implemented: create a session, subscribe in MERGE mode, decode updates. Everything else
//! the server may send is parsed into a typed variant or [`Notification::Other`] and never dropped
//! silently. Wire details are from the public TLCP 2.5.0 specification.

use thiserror::Error;

/// WebSocket subprotocol that selects TLCP 2.5.0.
pub const SUBPROTOCOL: &str = "TLCP-2.5.0.lightstreamer.com";
/// Client id every generic TLCP client sends; it identifies the protocol family, not a user.
const GENERIC_CLIENT_ID: &str = "mgQkwtwdysogQz2BJ4Ji kOj2Bg";
/// Line separator for requests and notifications.
const CRLF: &str = "\r\n";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TlcpError {
    #[error("malformed notification {line:?}: {reason}")]
    Malformed { line: String, reason: &'static str },
    #[error("bad percent-encoding in {0:?}")]
    BadPercent(String),
    #[error("update has {got} fields but the subscription schema has {want}")]
    FieldCount { got: usize, want: usize },
    #[error("update uses diff format ^{0}, which this client does not implement")]
    UnsupportedDiff(char),
    #[error("update repeat marker {0:?} is not a number")]
    BadRepeat(String),
}

/// Percent-encode everything but RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decode `%XX` sequences as UTF-8.
pub fn percent_decode(s: &str) -> Result<String, TlcpError> {
    let bad = || TlcpError::BadPercent(s.to_owned());
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or_else(bad)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| bad())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| bad())
}

/// `create_session` request for the given adapter set.
pub fn create_session_request(adapter_set: &str) -> String {
    format!(
        "create_session{CRLF}LS_adapter_set={}&LS_cid={}",
        percent_encode(adapter_set),
        percent_encode(GENERIC_CLIENT_ID)
    )
}

/// Parameters of a MERGE-mode subscription.
pub struct Subscribe<'a> {
    pub req_id: u32,
    /// Progressive from 1, unique within the session.
    pub sub_id: u32,
    pub items: &'a [String],
    pub fields: &'a [String],
    /// Ask the server for the current value of every item immediately.
    pub snapshot: bool,
    /// Data adapter within the adapter set; `None` means the set's `DEFAULT`.
    pub data_adapter: Option<&'a str>,
}

/// `control` request adding a MERGE subscription. Item and field lists are space-separated names,
/// which the default metadata adapter resolves.
pub fn subscribe_request(s: &Subscribe<'_>) -> String {
    let adapter = s.data_adapter.map_or(String::new(), |a| {
        format!("&LS_data_adapter={}", percent_encode(a))
    });
    format!(
        "control{CRLF}LS_reqId={}&LS_op=add&LS_subId={}&LS_mode=MERGE&LS_group={}&LS_schema={}&LS_snapshot={}{adapter}",
        s.req_id,
        s.sub_id,
        percent_encode(&s.items.join(" ")),
        percent_encode(&s.fields.join(" ")),
        s.snapshot
    )
}

/// `control` request that ends the session cleanly (cause code 0 = client-initiated).
pub fn destroy_request(req_id: u32) -> String {
    format!("control{CRLF}LS_reqId={req_id}&LS_op=destroy&LS_cause_code=0")
}

/// One server notification (one line of a WebSocket text message).
#[derive(Debug, PartialEq, Eq)]
pub enum Notification {
    WsOk,
    ConOk {
        session_id: String,
        request_limit: u64,
        keepalive_ms: u64,
        control_link: String,
    },
    ConErr {
        code: i64,
        message: String,
    },
    ReqOk {
        req_id: u32,
    },
    ReqErr {
        req_id: u32,
        code: i64,
        message: String,
    },
    Error {
        code: i64,
        message: String,
    },
    SubOk {
        sub_id: u32,
        items: usize,
        fields: usize,
    },
    /// `values` is the still-encoded pipe-separated list; decode with [`FieldState::apply`].
    Update {
        sub_id: u32,
        item: usize,
        values: String,
    },
    Probe,
    Loop {
        expected_delay_ms: u64,
    },
    End {
        code: i64,
        message: String,
    },
    /// Informational notifications (`SERVNAME`, `CLIENTIP`, `CONS`, `SYNC`, `PROG`, `NOOP`, `EOS`, `CS`, `OV`, `UNSUB`, `CONF`, ...).
    Other {
        tag: String,
        args: String,
    },
}

fn malformed(line: &str, reason: &'static str) -> TlcpError {
    TlcpError::Malformed {
        line: line.to_owned(),
        reason,
    }
}

fn num<T: std::str::FromStr>(line: &str, s: &str) -> Result<T, TlcpError> {
    s.parse()
        .map_err(|_| malformed(line, "numeric argument expected"))
}

/// Parse one notification line (without its CR-LF).
///
/// Per the spec, arguments are split on commas except the last argument of `U`, and non-numeric
/// arguments are percent-decoded, again except the value list of `U`.
pub fn parse_notification(line: &str) -> Result<Notification, TlcpError> {
    let (tag, rest) = line.split_once(',').unwrap_or((line, ""));
    let args = |n: usize| -> Result<Vec<&str>, TlcpError> {
        let v: Vec<&str> = rest.splitn(n, ',').collect();
        if v.len() == n {
            Ok(v)
        } else {
            Err(malformed(line, "too few arguments"))
        }
    };
    Ok(match tag {
        "WSOK" => Notification::WsOk,
        "PROBE" => Notification::Probe,
        "CONOK" => {
            let a = args(4)?;
            Notification::ConOk {
                session_id: a[0].to_owned(),
                request_limit: num(line, a[1])?,
                keepalive_ms: num(line, a[2])?,
                control_link: a[3].to_owned(),
            }
        }
        "CONERR" | "ERROR" | "END" => {
            let a = args(2)?;
            let (code, message) = (num(line, a[0])?, percent_decode(a[1])?);
            match tag {
                "CONERR" => Notification::ConErr { code, message },
                "ERROR" => Notification::Error { code, message },
                _ => Notification::End { code, message },
            }
        }
        "REQOK" => Notification::ReqOk {
            req_id: num(line, args(1)?[0])?,
        },
        "REQERR" => {
            let a = args(3)?;
            Notification::ReqErr {
                req_id: num(line, a[0])?,
                code: num(line, a[1])?,
                message: percent_decode(a[2])?,
            }
        }
        "SUBOK" => {
            let a = args(3)?;
            Notification::SubOk {
                sub_id: num(line, a[0])?,
                items: num(line, a[1])?,
                fields: num(line, a[2])?,
            }
        }
        "U" => {
            let a = args(3)?;
            Notification::Update {
                sub_id: num(line, a[0])?,
                item: num(line, a[1])?,
                values: a[2].to_owned(),
            }
        }
        "LOOP" => Notification::Loop {
            expected_delay_ms: num(line, args(1)?[0])?,
        },
        _ => Notification::Other {
            tag: tag.to_owned(),
            args: rest.to_owned(),
        },
    })
}

/// Current field values of one item of a MERGE subscription.
///
/// `None` is a protocol null; `Some("")` is an empty string. They are distinct on the wire.
pub struct FieldState {
    values: Vec<Option<String>>,
}

impl FieldState {
    pub fn new(field_count: usize) -> Self {
        Self {
            values: vec![None; field_count],
        }
    }

    pub fn values(&self) -> &[Option<String>] {
        &self.values
    }

    /// Apply a pipe-separated update list, returning the indexes of fields it touched.
    ///
    /// Tokens: empty = unchanged, `#` = null, `$` = empty string, `^N` = N unchanged fields,
    /// `^P`/`^T` = diff formats (rejected), otherwise percent-encoded content.
    /// The state is left untouched if the update is rejected.
    pub fn apply(&mut self, list: &str) -> Result<Vec<usize>, TlcpError> {
        let want = self.values.len();
        let mut next = self.values.clone();
        let mut changed = Vec::new();
        let mut at = 0usize;
        for token in list.split('|') {
            if at >= want {
                return Err(TlcpError::FieldCount { got: at + 1, want });
            }
            match token {
                "" => at += 1,
                "#" => {
                    next[at] = None;
                    changed.push(at);
                    at += 1;
                }
                "$" => {
                    next[at] = Some(String::new());
                    changed.push(at);
                    at += 1;
                }
                t if t.starts_with('^') => {
                    let marker = &t[1..];
                    match marker.chars().next() {
                        Some(c) if c.is_ascii_digit() => {
                            let n: usize = marker
                                .parse()
                                .map_err(|_| TlcpError::BadRepeat(t.to_owned()))?;
                            at += n;
                        }
                        Some(c) => return Err(TlcpError::UnsupportedDiff(c)),
                        None => return Err(TlcpError::BadRepeat(t.to_owned())),
                    }
                }
                t => {
                    next[at] = Some(percent_decode(t)?);
                    changed.push(at);
                    at += 1;
                }
            }
        }
        if at != want {
            return Err(TlcpError::FieldCount { got: at, want });
        }
        self.values = next;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(s: &str) -> Option<String> {
        Some(s.to_owned())
    }

    // Fixtures below are the worked examples from the TLCP 2.5.0 specification (stock-quote
    // subscription 3, ten fields).
    const SCHEMA: usize = 10;

    #[test]
    fn spec_example_snapshot_with_null_and_empty() {
        let mut s = FieldState::new(SCHEMA);
        s.apply("20:00:33|3.04|0.0|2.41|3.67|3.03|3.04|#|#|$")
            .unwrap();
        assert_eq!(s.values()[0], some("20:00:33"));
        assert_eq!(s.values()[7], None, "# is null");
        assert_eq!(s.values()[9], some(""), "$ is empty, not null");
    }

    #[test]
    fn spec_example_unchanged_fields_and_repeat_markers() {
        let mut s = FieldState::new(SCHEMA);
        s.apply("20:00:33|3.04|0.0|2.41|3.67|3.03|3.04|#|#|$")
            .unwrap();
        assert_eq!(
            s.apply("20:00:54|3.07|0.98|||3.06|3.07|||Suspended")
                .unwrap(),
            [0, 1, 2, 5, 6, 9]
        );
        assert_eq!(
            s.values()[3],
            some("2.41"),
            "empty token leaves the field unchanged"
        );
        assert_eq!(s.values()[9], some("Suspended"));
        s.apply("20:04:40|^4|3.02|3.03|||").unwrap();
        assert_eq!(
            s.values()[1],
            some("3.07"),
            "^4 leaves price..maximum unchanged"
        );
        assert_eq!(s.values()[5], some("3.02"));
        s.apply("20:06:10|3.05|0.32|^7").unwrap();
        assert_eq!(s.values()[0], some("20:06:10"));
        assert_eq!(s.values()[1], some("3.05"));
        assert_eq!(
            s.values()[5],
            some("3.02"),
            "^7 covers the last seven fields"
        );
    }

    #[test]
    fn rejected_update_leaves_state_untouched() {
        let mut s = FieldState::new(3);
        s.apply("a|b|c").unwrap();
        assert!(s.apply("x|y").is_err(), "too few fields");
        assert!(s.apply("x|y|z|w").is_err(), "too many fields");
        assert!(s.apply("x|^T1abc|z").is_err(), "diff format");
        assert_eq!(s.values(), [some("a"), some("b"), some("c")]);
    }

    #[test]
    fn repeat_marker_overrun_is_an_error() {
        let mut s = FieldState::new(3);
        s.apply("a|b|c").unwrap();
        assert!(matches!(s.apply("^5"), Err(TlcpError::FieldCount { .. })));
        assert!(matches!(
            s.apply("^x"),
            Err(TlcpError::UnsupportedDiff('x'))
        ));
        assert!(matches!(s.apply("^"), Err(TlcpError::BadRepeat(_))));
    }

    #[test]
    fn content_is_percent_decoded_after_splitting() {
        let mut s = FieldState::new(2);
        s.apply("aa%7Cbb|%23hash").unwrap();
        assert_eq!(s.values(), [some("aa|bb"), some("#hash")]);
    }

    #[test]
    fn percent_roundtrip_and_errors() {
        let raw = "mgQkwtwdysogQz2BJ4Ji kOj2Bg&=+%";
        assert_eq!(percent_decode(&percent_encode(raw)).unwrap(), raw);
        assert_eq!(percent_encode("a b"), "a%20b");
        assert!(percent_decode("%4").is_err());
        assert!(percent_decode("%zz").is_err());
        assert!(percent_decode("%FF").is_err(), "invalid UTF-8");
    }

    #[test]
    fn requests_have_the_documented_shape() {
        assert_eq!(
            create_session_request("ISSLIVE"),
            "create_session\r\nLS_adapter_set=ISSLIVE&LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"
        );
        let items = vec!["A".to_owned(), "B".to_owned()];
        let fields = vec!["TimeStamp".to_owned(), "Value".to_owned()];
        let r = subscribe_request(&Subscribe {
            req_id: 1,
            sub_id: 1,
            items: &items,
            fields: &fields,
            snapshot: true,
            data_adapter: None,
        });
        assert_eq!(
            r,
            "control\r\nLS_reqId=1&LS_op=add&LS_subId=1&LS_mode=MERGE&LS_group=A%20B&LS_schema=TimeStamp%20Value&LS_snapshot=true"
        );
        let r = subscribe_request(&Subscribe {
            req_id: 2,
            sub_id: 2,
            items: &items,
            fields: &fields,
            snapshot: false,
            data_adapter: Some("QUOTE ADAPTER"),
        });
        assert!(r.ends_with("&LS_snapshot=false&LS_data_adapter=QUOTE%20ADAPTER"));
        assert_eq!(
            destroy_request(9),
            "control\r\nLS_reqId=9&LS_op=destroy&LS_cause_code=0"
        );
    }

    #[test]
    fn notifications_parse() {
        assert_eq!(
            parse_notification("CONOK,S73d162c183916f0dT2729905,50000,5000,*").unwrap(),
            Notification::ConOk {
                session_id: "S73d162c183916f0dT2729905".into(),
                request_limit: 50000,
                keepalive_ms: 5000,
                control_link: "*".into()
            }
        );
        assert_eq!(
            parse_notification("SUBOK,3,1,10").unwrap(),
            Notification::SubOk {
                sub_id: 3,
                items: 1,
                fields: 10
            }
        );
        assert_eq!(
            parse_notification("CONERR,2,Requested Adapter Set not available").unwrap(),
            Notification::ConErr {
                code: 2,
                message: "Requested Adapter Set not available".into()
            }
        );
        assert_eq!(
            parse_notification("REQERR,19,0,Specified%20subscription%20not%20found").unwrap(),
            Notification::ReqErr {
                req_id: 19,
                code: 0,
                message: "Specified subscription not found".into()
            }
        );
        assert_eq!(parse_notification("WSOK").unwrap(), Notification::WsOk);
        assert_eq!(parse_notification("PROBE").unwrap(), Notification::Probe);
        assert_eq!(
            parse_notification("U,3,1,20:00:33|3.04,with,commas").unwrap(),
            Notification::Update {
                sub_id: 3,
                item: 1,
                values: "20:00:33|3.04,with,commas".into()
            },
            "the last argument of U may contain commas"
        );
        assert!(matches!(
            parse_notification("SERVNAME,Lightstreamer HTTP Server").unwrap(),
            Notification::Other { .. }
        ));
    }

    #[test]
    fn malformed_notifications_are_errors_not_panics() {
        for bad in [
            "CONOK,S1,x,5000,*",
            "CONOK,S1",
            "SUBOK,1,2",
            "U,1",
            "U,a,1,x",
            "REQOK",
            "END,x,msg",
            "LOOP,soon",
        ] {
            assert!(parse_notification(bad).is_err(), "{bad}");
        }
    }
}

//! Pure core of the key-injecting relay (docs/PORTS.md, "Key-injecting relay").
//!
//! A container that uses an API-key account holds only a per-attempt
//! placeholder. Its CLI sends each request to the relay, which parses the head
//! once into typed parts ([`ParsedHead`]), checks it against its backend's fixed
//! [`InjectionRule`] and the attempt's [`AttemptCap`], and returns the outbound
//! head as data ([`plan`]) with a [`SecretSlot`] where the edge splices the real
//! key. This crate never sees the key and does no I/O; the host's edge
//! (`src/ports/inject_edge.rs`) does the reading, connecting and writing.
use std::{borrow::Cow, fmt, num::NonZeroU16};

mod limits;
pub use limits::{Budget, Deny, Limits, Line, Millis, Retention, Ticket, prune};

/// The largest request head the relay reads.
pub const HEAD_LIMIT: usize = 16 * 1024;
/// The largest request body the relay forwards.
pub const BODY_LIMIT: u64 = 32 * 1024 * 1024;

/// Declares [`Refusal`] with each variant's status and audit name on one line.
macro_rules! refusals {
    ($($(#[$doc:meta])* $variant:ident => $status:ident, $name:literal;)*) => {
        /// Why a request is not relayed. Each maps to one status
        /// ([`Refusal::status`]) and one stable name for the audit log
        /// ([`Refusal::name`]); neither quotes the request.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Refusal {
            $($(#[$doc])* $variant,)*
        }
        impl Refusal {
            pub fn status(self) -> Status {
                match self {
                    $(Self::$variant => Status::$status,)*
                }
            }
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}
refusals! {
    HeadTooLarge => HeaderFieldsTooLarge, "head_too_large";
    IncompleteHead => BadRequest, "incomplete_head";
    BareLineBreak => BadRequest, "bare_line_break";
    MalformedRequestLine => BadRequest, "malformed_request_line";
    UnsupportedMethod => Forbidden, "unsupported_method";
    AbsoluteForm => BadRequest, "absolute_form";
    AuthorityForm => BadRequest, "authority_form";
    InvalidPath => BadRequest, "invalid_path";
    DotSegment => BadRequest, "dot_segment";
    EncodedSeparator => BadRequest, "encoded_separator";
    InvalidQuery => BadRequest, "invalid_query";
    UnsupportedVersion => BadRequest, "unsupported_version";
    ObsFold => BadRequest, "obs_fold";
    MalformedHeader => BadRequest, "malformed_header";
    EmptyHeaderName => BadRequest, "empty_header_name";
    NonAsciiHeaderName => BadRequest, "non_ascii_header_name";
    InvalidHeaderName => BadRequest, "invalid_header_name";
    WhitespaceBeforeColon => BadRequest, "whitespace_before_colon";
    InvalidHeaderValue => BadRequest, "invalid_header_value";
    MissingHost => BadRequest, "missing_host";
    DuplicateHost => BadRequest, "duplicate_host";
    DuplicateContentLength => BadRequest, "duplicate_content_length";
    InvalidContentLength => BadRequest, "invalid_content_length";
    MissingContentLength => LengthRequired, "missing_content_length";
    TransferEncoding => LengthRequired, "transfer_encoding";
    TransferEncodingWithLength => BadRequest, "transfer_encoding_with_length";
    Upgrade => BadRequest, "upgrade";
    BodyTooLarge => PayloadTooLarge, "body_too_large";
    RouteNotAllowed => Forbidden, "route_not_allowed";
    QueryNotAllowed => Forbidden, "query_not_allowed";
    MissingCredential => Forbidden, "missing_credential";
    DuplicateCredential => BadRequest, "duplicate_credential";
    WrongCredential => Forbidden, "wrong_credential";
    /// The placeholder appears outside the credential headers, where it
    /// would travel upstream.
    PlaceholderLeak => BadRequest, "placeholder_leak";
}

/// The statuses a refusal answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    BadRequest,
    Forbidden,
    LengthRequired,
    PayloadTooLarge,
    HeaderFieldsTooLarge,
}
impl Status {
    pub fn code(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Forbidden => 403,
            Self::LengthRequired => 411,
            Self::PayloadTooLarge => 413,
            Self::HeaderFieldsTooLarge => 431,
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::BadRequest => "Bad Request",
            Self::Forbidden => "Forbidden",
            Self::LengthRequired => "Length Required",
            Self::PayloadTooLarge => "Payload Too Large",
            Self::HeaderFieldsTooLarge => "Request Header Fields Too Large",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}
impl Method {
    fn parse(token: &[u8]) -> Result<Self, Refusal> {
        match token {
            b"GET" => Ok(Self::Get),
            b"POST" => Ok(Self::Post),
            _ => Err(Refusal::UnsupportedMethod),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
    /// Whether the method carries a body, so the head must say how long.
    fn has_body(self) -> bool {
        match self {
            Self::Get => false,
            Self::Post => true,
        }
    }
}

/// The only version relayed: one request per connection, no chunked bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    Http11,
}

/// One path segment: non-empty, `.`/`..` free, no encoded separator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment(String);
/// One `name=value` query parameter, kept as sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    name: String,
    value: String,
}
/// A validated origin-form target: `/segment/...` with an optional query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginForm {
    segments: Vec<Segment>,
    query: Vec<Param>,
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}
fn segment(text: &str) -> Result<Segment, Refusal> {
    if text.is_empty() {
        return Err(Refusal::InvalidPath);
    }
    if text == "." || text == ".." {
        return Err(Refusal::DotSegment);
    }
    let bytes = text.as_bytes();
    let escape = |at: usize| {
        bytes
            .get(at + 1..at + 3)
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    };
    bytes.iter().enumerate().try_for_each(|(at, &byte)| match byte {
        b'%' => match escape(at).map(|hex| hex.to_ascii_lowercase()) {
            Some(hex) if hex == b"2f" || hex == b"5c" => Err(Refusal::EncodedSeparator),
            // An encoded dot could rebuild a dot segment after decoding.
            Some(hex) if hex == b"2e" => Err(Refusal::DotSegment),
            Some(_) => Ok(()),
            None => Err(Refusal::InvalidPath),
        },
        byte if unreserved(byte) || byte == b':' || byte == b'@' => Ok(()),
        // Hex digits of an escape already checked above.
        _ => Err(Refusal::InvalidPath),
    })?;
    Ok(Segment(text.to_owned()))
}
fn param(text: &str) -> Result<Param, Refusal> {
    let (name, value) = text.split_once('=').ok_or(Refusal::InvalidQuery)?;
    let plain = |part: &str| part.bytes().all(unreserved);
    if name.is_empty() || !plain(name) || !plain(value) {
        return Err(Refusal::InvalidQuery);
    }
    Ok(Param {
        name: name.to_owned(),
        value: value.to_owned(),
    })
}
impl OriginForm {
    fn parse(target: &str) -> Result<Self, Refusal> {
        let Some(rest) = target.strip_prefix('/') else {
            return Err(if target.contains("://") {
                Refusal::AbsoluteForm
            } else {
                Refusal::AuthorityForm
            });
        };
        if rest.contains('#') {
            return Err(Refusal::InvalidPath);
        }
        let (path, query) = match rest.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (rest, None),
        };
        Ok(Self {
            segments: path.split('/').map(segment).collect::<Result<_, _>>()?,
            query: query.map_or(Ok(Vec::new()), |query| query.split('&').map(param).collect())?,
        })
    }
    fn render(&self, strip_query: &[&str]) -> String {
        let path: String = self.segments.iter().map(|s| format!("/{}", s.0)).collect();
        let query: Vec<String> = self
            .query
            .iter()
            .filter(|p| !strip_query.contains(&p.name.as_str()))
            .map(|p| format!("{}={}", p.name, p.value))
            .collect();
        if query.is_empty() {
            path
        } else {
            format!("{path}?{}", query.join("&"))
        }
    }
}

/// A header name; compared without regard to case.
#[derive(Clone, Debug)]
pub struct HeaderName(Cow<'static, str>);
impl HeaderName {
    /// A name the rule table uses: a lower-case token, checked at compile time.
    pub const fn known(name: &'static str) -> Self {
        let bytes = name.as_bytes();
        assert!(!bytes.is_empty());
        let mut at = 0;
        while at < bytes.len() {
            assert!(bytes[at].is_ascii_lowercase() || bytes[at].is_ascii_digit() || bytes[at] == b'-');
            at += 1;
        }
        Self(Cow::Borrowed(name))
    }
    fn parse(bytes: &[u8]) -> Result<Self, Refusal> {
        match bytes {
            [] => Err(Refusal::EmptyHeaderName),
            [.., b' ' | b'\t'] => Err(Refusal::WhitespaceBeforeColon),
            bytes if !bytes.is_ascii() => Err(Refusal::NonAsciiHeaderName),
            bytes if !bytes.iter().all(|&b| token(b)) => Err(Refusal::InvalidHeaderName),
            bytes => Ok(Self(Cow::Owned(String::from_utf8_lossy(bytes).into_owned()))),
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl PartialEq for HeaderName {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}
impl Eq for HeaderName {}
/// RFC 9110 `tchar`.
fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// A header value without surrounding whitespace: printable ASCII, spaces and tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderValue(String);
impl HeaderValue {
    fn parse(bytes: &[u8]) -> Result<Self, Refusal> {
        let value = bytes.trim_ascii();
        if !value.iter().all(|&b| b.is_ascii_graphic() || b == b' ' || b == b'\t') {
            return Err(Refusal::InvalidHeaderValue);
        }
        Ok(Self(String::from_utf8_lossy(value).into_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The request's headers in the order sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Headers(Vec<(HeaderName, HeaderValue)>);
impl Headers {
    pub fn all<'a>(&'a self, name: &HeaderName) -> impl Iterator<Item = &'a HeaderValue> {
        self.0.iter().filter(move |(n, _)| n == name).map(|(_, v)| v)
    }
    fn at_most_one(&self, name: &HeaderName, duplicate: Refusal) -> Result<Option<&HeaderValue>, Refusal> {
        let mut values = self.all(name);
        match (values.next(), values.next()) {
            (first, None) => Ok(first),
            (Some(_), Some(_)) | (None, Some(_)) => Err(duplicate),
        }
    }
}

pub const AUTHORIZATION: HeaderName = HeaderName::known("authorization");
pub const X_API_KEY: HeaderName = HeaderName::known("x-api-key");
pub const X_GOOG_API_KEY: HeaderName = HeaderName::known("x-goog-api-key");
const HOST: HeaderName = HeaderName::known("host");
const CONTENT_LENGTH: HeaderName = HeaderName::known("content-length");
const TRANSFER_ENCODING: HeaderName = HeaderName::known("transfer-encoding");
const UPGRADE: HeaderName = HeaderName::known("upgrade");

/// One request head, parsed once. Everything after works on these parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedHead {
    pub method: Method,
    pub target: OriginForm,
    pub version: Version,
    pub headers: Headers,
    pub content_length: u64,
}
impl ParsedHead {
    /// Parses `head`, which ends with its blank line and holds nothing after it.
    pub fn parse(head: &[u8]) -> Result<Self, Refusal> {
        if head.len() > HEAD_LIMIT {
            return Err(Refusal::HeadTooLarge);
        }
        let body = head.strip_suffix(b"\r\n\r\n").ok_or(Refusal::IncompleteHead)?;
        let lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();
        // Every line break is CRLF: a lone CR or LF is where smuggling starts.
        let lines = lines
            .iter()
            .enumerate()
            .map(|(at, line)| match (at + 1 == lines.len(), line.strip_suffix(b"\r")) {
                (true, None) => Ok(*line),
                (false, Some(line)) => Ok(line),
                (true, Some(_)) | (false, None) => Err(Refusal::BareLineBreak),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if lines.iter().any(|line| line.contains(&b'\r')) {
            return Err(Refusal::BareLineBreak);
        }
        let (request, fields) = lines.split_first().ok_or(Refusal::MalformedRequestLine)?;
        let (method, target, version) = request_line(request)?;
        let headers = Headers(fields.iter().map(|line| field(line)).collect::<Result<_, _>>()?);
        headers
            .at_most_one(&HOST, Refusal::DuplicateHost)?
            .ok_or(Refusal::MissingHost)?;
        let length = headers.at_most_one(&CONTENT_LENGTH, Refusal::DuplicateContentLength)?;
        match (headers.all(&TRANSFER_ENCODING).next(), length) {
            (Some(_), Some(_)) => return Err(Refusal::TransferEncodingWithLength),
            (Some(_), None) => return Err(Refusal::TransferEncoding),
            (None, _) => {}
        }
        if headers.all(&UPGRADE).next().is_some() {
            return Err(Refusal::Upgrade);
        }
        let content_length = match length {
            Some(value) => content_length(value)?,
            None if method.has_body() => return Err(Refusal::MissingContentLength),
            None => 0,
        };
        if content_length > BODY_LIMIT {
            return Err(Refusal::BodyTooLarge);
        }
        Ok(Self {
            method,
            target,
            version,
            headers,
            content_length,
        })
    }
}
fn request_line(line: &[u8]) -> Result<(Method, OriginForm, Version), Refusal> {
    let parts: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
    let [method, target, version] = parts[..] else {
        return Err(Refusal::MalformedRequestLine);
    };
    let method = Method::parse(method)?;
    let target = std::str::from_utf8(target)
        .ok()
        .filter(|t| t.is_ascii())
        .ok_or(Refusal::InvalidPath)?;
    let target = OriginForm::parse(target)?;
    match version {
        b"HTTP/1.1" => Ok((method, target, Version::Http11)),
        _ => Err(Refusal::UnsupportedVersion),
    }
}
fn field(line: &[u8]) -> Result<(HeaderName, HeaderValue), Refusal> {
    if line.first().is_some_and(|b| matches!(b, b' ' | b'\t')) {
        return Err(Refusal::ObsFold);
    }
    let colon = line.iter().position(|&b| b == b':').ok_or(Refusal::MalformedHeader)?;
    Ok((
        HeaderName::parse(&line[..colon])?,
        HeaderValue::parse(&line[colon + 1..])?,
    ))
}
fn content_length(value: &HeaderValue) -> Result<u64, Refusal> {
    let text = value.as_str();
    if text.is_empty() || text.len() > 19 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Refusal::InvalidContentLength);
    }
    text.parse().map_err(|_| Refusal::InvalidContentLength)
}

/// A provider's fixed host name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host(&'static str);
/// A provider's fixed TCP port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port(NonZeroU16);
/// The one HTTPS origin a rule reaches. Nothing in a request can change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    host: Host,
    port: Port,
}
impl Origin {
    pub const fn https(host: &'static str, port: u16) -> Self {
        let Some(port) = NonZeroU16::new(port) else {
            panic!("an origin needs a port")
        };
        Self {
            host: Host(host),
            port: Port(port),
        }
    }
    pub fn host(&self) -> &'static str {
        self.host.0
    }
    pub fn port(&self) -> u16 {
        self.port.0.get()
    }
    /// The `Host` header value.
    fn authority(&self) -> String {
        match self.port() {
            443 => self.host().to_owned(),
            port => format!("{}:{port}", self.host()),
        }
    }
}

/// How the key is written into the header the rule sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// `Bearer KEY`.
    Bearer,
    /// The key alone.
    Raw,
}
impl Scheme {
    fn prefix(self) -> &'static str {
        match self {
            Self::Bearer => "Bearer ",
            Self::Raw => "",
        }
    }
    /// The credential inside a presented value, if it has this scheme's form.
    fn credential(self, value: &str) -> Option<&str> {
        match self {
            Self::Bearer => value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, credential)| credential.trim_start()),
            Self::Raw => Some(value),
        }
    }
}

/// A header the relay never forwards.
#[derive(Clone, Debug)]
pub enum Strip {
    Name(HeaderName),
    /// Every name starting with this lower-case prefix.
    Prefix(&'static str),
}
impl Strip {
    fn matches(&self, name: &HeaderName) -> bool {
        match self {
            Self::Name(known) => known == name,
            Self::Prefix(prefix) => name
                .as_str()
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix)),
        }
    }
}
/// The headers every rule strips: inbound credentials, hop-by-hop and
/// framing headers (the relay writes its own `Host`, `Content-Length` and
/// `Connection`).
pub const STRIP: &[Strip] = &[
    Strip::Name(AUTHORIZATION),
    Strip::Name(X_API_KEY),
    Strip::Name(X_GOOG_API_KEY),
    Strip::Prefix("proxy-"),
    Strip::Name(UPGRADE),
    Strip::Name(HeaderName::known("connection")),
    Strip::Name(HeaderName::known("keep-alive")),
    Strip::Name(HeaderName::known("te")),
    Strip::Name(HeaderName::known("trailer")),
    Strip::Name(TRANSFER_ENCODING),
    Strip::Name(HOST),
    Strip::Name(CONTENT_LENGTH),
];
/// The headers a CLI may present its placeholder in, and in which form. Every
/// one present must hold the attempt's placeholder.
const CREDENTIALS: [(HeaderName, Scheme); 3] = [
    (AUTHORIZATION, Scheme::Bearer),
    (X_API_KEY, Scheme::Raw),
    (X_GOOG_API_KEY, Scheme::Raw),
];

/// What a query parameter's value may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryValue {
    Exact(&'static str),
    Digits,
    /// Letters, digits, `.`, `_`, `-`, `~` (a version string).
    Token,
}
/// One query parameter a route allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allowed {
    pub name: &'static str,
    pub value: QueryValue,
}
impl Allowed {
    fn admits(&self, param: &Param) -> bool {
        param.name == self.name
            && match self.value {
                QueryValue::Exact(value) => param.value == value,
                QueryValue::Digits => !param.value.is_empty() && param.value.bytes().all(|b| b.is_ascii_digit()),
                QueryValue::Token => !param.value.is_empty(),
            }
    }
}

/// The last segment of a [`PathPattern`] that varies: `{name}` followed by
/// a fixed `suffix` (`{model}:streamGenerateContent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Named {
    pub name: &'static str,
    pub suffix: &'static str,
}
/// Literal segments, optionally followed by one [`Named`] segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathPattern {
    pub literal: &'static [&'static str],
    pub named: Option<Named>,
}
impl PathPattern {
    fn matches(&self, segments: &[Segment]) -> bool {
        let fixed = self.literal.len();
        let named = segments.get(fixed..).unwrap_or_default();
        segments.len() >= fixed
            && segments.iter().zip(self.literal).all(|(s, l)| s.0 == *l)
            && match (self.named, named) {
                (None, []) => true,
                (Some(Named { suffix, .. }), [last]) => last.0.strip_suffix(suffix).is_some_and(|stem| {
                    !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                }),
                (None, [_, ..]) | (Some(_), _) => false,
            }
    }
}
/// The template, which is also the route's class in the audit log.
impl fmt::Display for PathPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.literal.iter().try_for_each(|s| write!(f, "/{s}"))?;
        self.named
            .map_or(Ok(()), |Named { name, suffix }| write!(f, "/{{{name}}}{suffix}"))
    }
}
/// One allowed request: method, path and the query parameters it may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    pub path: PathPattern,
    pub query: &'static [Allowed],
}

/// How the placeholder a CLI holds looks: some CLIs check the key's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `sk-proj-ctl-<hex>`: Codex expects an OpenAI project key.
    OpenAiProject,
    /// `ctl-<hex>`: the other CLIs accept any non-empty key.
    Plain,
}
impl Shape {
    fn prefix(self) -> &'static str {
        match self {
            Self::OpenAiProject => "sk-proj-ctl-",
            Self::Plain => "ctl-",
        }
    }
}

/// One backend's fixed relay rule.
#[derive(Clone, Debug)]
pub struct InjectionRule {
    pub origin: Origin,
    /// Everything else is refused.
    pub routes: &'static [Route],
    /// The header the real key goes in.
    pub set: HeaderName,
    pub scheme: Scheme,
    pub strip: &'static [Strip],
    /// Query parameters removed before forwarding, whatever their value.
    pub strip_query: &'static [&'static str],
    pub placeholder: Shape,
}
impl InjectionRule {
    fn route(&self, method: Method, target: &OriginForm) -> Result<&Route, Refusal> {
        let route = self
            .routes
            .iter()
            .find(|route| route.method == method && route.path.matches(&target.segments))
            .ok_or(Refusal::RouteNotAllowed)?;
        let forwarded: Vec<&Param> = target
            .query
            .iter()
            .filter(|p| !self.strip_query.contains(&p.name.as_str()))
            .collect();
        let distinct = forwarded
            .iter()
            .enumerate()
            .all(|(at, p)| forwarded[..at].iter().all(|q| q.name != p.name));
        let allowed = forwarded.iter().all(|p| route.query.iter().any(|a| a.admits(p)));
        if distinct && allowed {
            Ok(route)
        } else {
            Err(Refusal::QueryNotAllowed)
        }
    }
}

const fn literal(segments: &'static [&'static str]) -> PathPattern {
    PathPattern {
        literal: segments,
        named: None,
    }
}
const fn model(suffix: &'static str) -> PathPattern {
    PathPattern {
        literal: &["v1beta", "models"],
        named: Some(Named { name: "model", suffix }),
    }
}
const fn route(method: Method, path: PathPattern, query: &'static [Allowed]) -> Route {
    Route { method, path, query }
}
const BETA: Allowed = Allowed {
    name: "beta",
    value: QueryValue::Exact("true"),
};

/// Codex: the Responses API.
pub static OPENAI: InjectionRule = InjectionRule {
    origin: Origin::https("api.openai.com", 443),
    routes: &[
        route(Method::Post, literal(&["v1", "responses"]), &[]),
        route(
            Method::Get,
            literal(&["v1", "models"]),
            &[Allowed {
                name: "client_version",
                value: QueryValue::Token,
            }],
        ),
    ],
    set: AUTHORIZATION,
    scheme: Scheme::Bearer,
    strip: STRIP,
    strip_query: &[],
    placeholder: Shape::OpenAiProject,
};
/// Claude Code sends its key in both `Authorization: Bearer` and `x-api-key`;
/// both are stripped and the key goes in `x-api-key`, the header Anthropic's
/// API takes API keys in (`Authorization` there is for OAuth tokens).
pub static ANTHROPIC: InjectionRule = InjectionRule {
    origin: Origin::https("api.anthropic.com", 443),
    routes: &[
        route(Method::Post, literal(&["v1", "messages"]), &[BETA]),
        route(Method::Post, literal(&["v1", "messages", "count_tokens"]), &[BETA]),
        route(
            Method::Get,
            literal(&["v1", "models"]),
            &[
                BETA,
                Allowed {
                    name: "limit",
                    value: QueryValue::Digits,
                },
            ],
        ),
    ],
    set: X_API_KEY,
    scheme: Scheme::Raw,
    strip: STRIP,
    strip_query: &[],
    placeholder: Shape::Plain,
};
/// Antigravity: the Gemini API. A `key=` query parameter is never forwarded.
pub static GEMINI: InjectionRule = InjectionRule {
    origin: Origin::https("generativelanguage.googleapis.com", 443),
    routes: &[
        route(
            Method::Post,
            model(":streamGenerateContent"),
            &[Allowed {
                name: "alt",
                value: QueryValue::Exact("sse"),
            }],
        ),
        route(Method::Post, model(":generateContent"), &[]),
        route(Method::Post, model(":countTokens"), &[]),
    ],
    set: X_GOOG_API_KEY,
    scheme: Scheme::Raw,
    strip: STRIP,
    strip_query: &["key"],
    placeholder: Shape::Plain,
};
/// Grok Build: xAI's Responses and Chat Completions APIs and its key probe.
pub static XAI: InjectionRule = InjectionRule {
    origin: Origin::https("api.x.ai", 443),
    routes: &[
        route(Method::Post, literal(&["v1", "responses"]), &[]),
        route(Method::Post, literal(&["v1", "chat", "completions"]), &[]),
        route(Method::Get, literal(&["v1", "api-key"]), &[]),
        route(Method::Get, literal(&["v1", "models"]), &[]),
    ],
    set: AUTHORIZATION,
    scheme: Scheme::Bearer,
    strip: STRIP,
    strip_query: &[],
    placeholder: Shape::Plain,
};

/// The random part of a placeholder. It cannot be printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 16]);
impl Token {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
    /// Compares without stopping at the first difference.
    fn same(&self, other: &Self) -> bool {
        self.0.iter().zip(&other.0).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
    }
}
impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(hidden)")
    }
}

/// The attempt a capability is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptId(pub String);

/// What one attempt's placeholder unlocks: one account's key, through one
/// relay, while the attempt runs. `A` is the host's account identifier.
#[derive(Debug)]
pub struct AttemptCap<A> {
    attempt: AttemptId,
    account: A,
    token: Token,
}
impl<A> AttemptCap<A> {
    pub fn new(attempt: AttemptId, account: A, token: Token) -> Self {
        Self {
            attempt,
            account,
            token,
        }
    }
    pub fn attempt(&self) -> &AttemptId {
        &self.attempt
    }
    pub fn account(&self) -> &A {
        &self.account
    }
    /// What the CLI is handed instead of the key.
    pub fn placeholder(&self, shape: Shape) -> Placeholder {
        Placeholder(format!("{}{}", shape.prefix(), self.token.hex()))
    }
    fn accepts(&self, presented: &str, shape: Shape) -> bool {
        Placeholder::parse(presented, shape).is_some_and(|token| token.same(&self.token))
    }
}

/// A placeholder in the shape a CLI needs. Not printable: it unlocks the
/// account through the relay while its attempt runs.
pub struct Placeholder(String);
impl Placeholder {
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// The token inside `text`, when it has `shape`'s form.
    pub fn parse(text: &str, shape: Shape) -> Option<Token> {
        let hex = text.strip_prefix(shape.prefix())?.as_bytes();
        if hex.len() != 32 {
            return None;
        }
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let bytes: Vec<u8> = hex
            .chunks(2)
            .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
            .collect::<Option<_>>()?;
        Some(Token(bytes.try_into().ok()?))
    }
}
impl fmt::Debug for Placeholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Placeholder(hidden)")
    }
}

/// Where in an outbound head the edge writes the key. Only [`Outbound::splice`]
/// uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretSlot(usize);
/// A key that cannot go in a header as is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSecret;

/// What the edge sends, decided by [`plan`]: data only, with no key in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outbound {
    pub origin: Origin,
    head: Vec<u8>,
    slot: SecretSlot,
    pub content_length: u64,
    /// The matched route's template, for the audit log.
    pub route: PathPattern,
}
impl Outbound {
    /// The head in three parts to write in order, with `secret` in the slot.
    /// A key that is not one printable ASCII word is refused, so it cannot add
    /// headers.
    pub fn splice<'a>(&'a self, secret: &'a [u8]) -> Result<[&'a [u8]; 3], InvalidSecret> {
        if secret.is_empty() || secret.len() > 4096 || !secret.iter().all(u8::is_ascii_graphic) {
            return Err(InvalidSecret);
        }
        let (before, after) = self.head.split_at(self.slot.0);
        Ok([before, secret, after])
    }
}

/// Decides whether `head` may go upstream under `rule` for the attempt `cap`
/// is bound to, and what is sent: the fixed origin, the head with `Host`
/// rewritten, every strip-list header and stripped query parameter removed,
/// `Connection: close`, and a [`SecretSlot`] for the key.
pub fn plan<A>(head: &ParsedHead, rule: &InjectionRule, cap: &AttemptCap<A>) -> Result<Outbound, Refusal> {
    let presented = CREDENTIALS
        .iter()
        .map(|(name, scheme)| {
            Ok(head
                .headers
                .at_most_one(name, Refusal::DuplicateCredential)?
                .map(|value| scheme.credential(value.as_str())))
        })
        .collect::<Result<Vec<_>, Refusal>>()?;
    let presented: Vec<Option<&str>> = presented.into_iter().flatten().collect();
    if presented.is_empty() {
        return Err(Refusal::MissingCredential);
    }
    if !presented
        .iter()
        .all(|credential| credential.is_some_and(|c| cap.accepts(c, rule.placeholder)))
    {
        return Err(Refusal::WrongCredential);
    }
    let route = rule.route(head.method, &head.target)?;
    let kept: String = head
        .headers
        .0
        .iter()
        .filter(|(name, _)| !rule.strip.iter().any(|strip| strip.matches(name)))
        .map(|(name, value)| format!("{}: {}\r\n", name.as_str(), value.as_str()))
        .collect();
    let length = if head.method.has_body() || head.content_length > 0 {
        format!("Content-Length: {}\r\n", head.content_length)
    } else {
        String::new()
    };
    let before = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\n{kept}{length}Connection: close\r\n{}: {}",
        head.method.as_str(),
        head.target.render(rule.strip_query),
        rule.origin.authority(),
        rule.set.as_str(),
        rule.scheme.prefix(),
    );
    let slot = SecretSlot(before.len());
    let head_bytes = [before.as_bytes(), b"\r\n\r\n"].concat();
    if head_bytes
        .to_ascii_lowercase()
        .windows(32)
        .any(|w| w == cap.token.hex().as_bytes())
    {
        return Err(Refusal::PlaceholderLeak);
    }
    Ok(Outbound {
        origin: rule.origin,
        head: head_bytes,
        slot,
        content_length: head.content_length,
        route: route.path,
    })
}

#[cfg(test)]
mod tests;

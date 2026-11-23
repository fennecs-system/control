use super::*;

const CANARY_TOKEN: [u8; 16] = *b"CANARY-TOKEN-016";

fn cap() -> AttemptCap<&'static str> {
    AttemptCap::new(
        AttemptId("attempt-1".into()),
        "claude+work",
        Token::from_bytes(CANARY_TOKEN),
    )
}
fn placeholder(rule: &InjectionRule) -> String {
    cap().placeholder(rule.placeholder).expose().to_owned()
}
fn head(text: &str) -> Vec<u8> {
    text.replace('\n', "\r\n").into_bytes()
}
/// A head for `rule` with its placeholder in the header the rule sets.
fn request(rule: &InjectionRule, method: &str, target: &str) -> Vec<u8> {
    let key = placeholder(rule);
    let credential = match rule.set.as_str() {
        "authorization" => format!("Authorization: Bearer {key}"),
        name => format!("{name}: {key}"),
    };
    let length = if method == "POST" { "Content-Length: 2\n" } else { "" };
    head(&format!(
        "{method} {target} HTTP/1.1\nHost: 127.0.0.1:3130\n{credential}\n{length}\n"
    ))
}
fn parse(bytes: &[u8]) -> Result<ParsedHead, Refusal> {
    ParsedHead::parse(bytes)
}
fn planned(rule: &InjectionRule, bytes: &[u8]) -> Result<Outbound, Refusal> {
    plan(&parse(bytes)?, rule, &cap())
}

#[test]
fn a_head_parses_once_into_typed_parts() {
    let parsed = parse(&head(
        "POST /v1/messages?beta=true HTTP/1.1\nHost: x\nX-Api-Key:  k \nContent-Length: 12\n\n",
    ))
    .unwrap();
    assert_eq!(parsed.method, Method::Post);
    assert_eq!(parsed.version, Version::Http11);
    assert_eq!(parsed.content_length, 12);
    assert_eq!(
        parsed.target.segments,
        vec![Segment("v1".into()), Segment("messages".into())]
    );
    assert_eq!(
        parsed.target.query,
        vec![Param {
            name: "beta".into(),
            value: "true".into()
        }]
    );
    let values: Vec<&str> = parsed.headers.all(&X_API_KEY).map(HeaderValue::as_str).collect();
    assert_eq!(values, ["k"], "names compare without case; values are trimmed");
    assert_eq!(
        parse(&head("GET /v1/models HTTP/1.1\nHost: x\n\n"))
            .unwrap()
            .content_length,
        0
    );
}

#[test]
fn every_malformed_or_ambiguous_head_is_refused_with_its_own_reason() {
    use Refusal::*;
    let big = format!(
        "GET /v1/models HTTP/1.1\r\nHost: x\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(HEAD_LIMIT)
    );
    let cases: &[(&[u8], Refusal)] = &[
        (big.as_bytes(), HeadTooLarge),
        (b"GET /v1/models HTTP/1.1\r\nHost: x\r\n", IncompleteHead),
        (b"GET /v1/models HTTP/1.1\nHost: x\r\n\r\n", BareLineBreak),
        (b"GET /v1/models HTTP/1.1\r\nHost: x\rY: z\r\n\r\n", BareLineBreak),
        (b"GET /v1/models HTTP/1.1\r\nHost: x\r\r\n\r\n", BareLineBreak),
        (b"GET  /v1/models HTTP/1.1\r\nHost: x\r\n\r\n", MalformedRequestLine),
        (b"GET /v1/models\r\nHost: x\r\n\r\n", MalformedRequestLine),
        (b"PUT /v1/models HTTP/1.1\r\nHost: x\r\n\r\n", UnsupportedMethod),
        (b"get /v1/models HTTP/1.1\r\nHost: x\r\n\r\n", UnsupportedMethod),
        (
            b"CONNECT api.openai.com:443 HTTP/1.1\r\nHost: x\r\n\r\n",
            UnsupportedMethod,
        ),
        (b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n", UnsupportedMethod),
        (
            b"GET https://evil.example/v1/models HTTP/1.1\r\nHost: x\r\n\r\n",
            AbsoluteForm,
        ),
        (b"GET api.openai.com:443 HTTP/1.1\r\nHost: x\r\n\r\n", AuthorityForm),
        (b"GET * HTTP/1.1\r\nHost: x\r\n\r\n", AuthorityForm),
        (b"GET /v1//models HTTP/1.1\r\nHost: x\r\n\r\n", InvalidPath),
        (b"GET /v1/models/ HTTP/1.1\r\nHost: x\r\n\r\n", InvalidPath),
        (b"GET /v1/mod\\els HTTP/1.1\r\nHost: x\r\n\r\n", InvalidPath),
        (b"GET /v1/models#x HTTP/1.1\r\nHost: x\r\n\r\n", InvalidPath),
        (b"GET /v1/m%zz HTTP/1.1\r\nHost: x\r\n\r\n", InvalidPath),
        (
            "GET /v1/mod\u{e9}ls HTTP/1.1\r\nHost: x\r\n\r\n".as_bytes(),
            InvalidPath,
        ),
        (b"GET /v1/../admin HTTP/1.1\r\nHost: x\r\n\r\n", DotSegment),
        (b"GET /v1/./models HTTP/1.1\r\nHost: x\r\n\r\n", DotSegment),
        (b"GET /v1/%2e%2e/admin HTTP/1.1\r\nHost: x\r\n\r\n", DotSegment),
        (b"GET /v1/%2Fmodels HTTP/1.1\r\nHost: x\r\n\r\n", EncodedSeparator),
        (b"GET /v1/%5cmodels HTTP/1.1\r\nHost: x\r\n\r\n", EncodedSeparator),
        (b"GET /v1/models?limit HTTP/1.1\r\nHost: x\r\n\r\n", InvalidQuery),
        (b"GET /v1/models? HTTP/1.1\r\nHost: x\r\n\r\n", InvalidQuery),
        (b"GET /v1/models?a=%2f HTTP/1.1\r\nHost: x\r\n\r\n", InvalidQuery),
        (b"GET /v1/models HTTP/1.0\r\nHost: x\r\n\r\n", UnsupportedVersion),
        (b"GET /v1/models HTTP/2\r\nHost: x\r\n\r\n", UnsupportedVersion),
        (b"GET /v1/models HTTP/1.1\r\nHost: x\r\nX-A: b\r\n c\r\n\r\n", ObsFold),
        (b"GET /v1/models HTTP/1.1\r\n Host: x\r\n\r\n", ObsFold),
        (
            b"GET /v1/models HTTP/1.1\r\nHost: x\r\nno colon\r\n\r\n",
            MalformedHeader,
        ),
        (b"GET /v1/models HTTP/1.1\r\nHost: x\r\n: v\r\n\r\n", EmptyHeaderName),
        (
            "GET /v1/models HTTP/1.1\r\nHost: x\r\nX-\u{e9}: v\r\n\r\n".as_bytes(),
            NonAsciiHeaderName,
        ),
        (
            b"GET /v1/models HTTP/1.1\r\nHost: x\r\nX(a): v\r\n\r\n",
            InvalidHeaderName,
        ),
        (b"GET /v1/models HTTP/1.1\r\nHost : x\r\n\r\n", WhitespaceBeforeColon),
        (
            b"GET /v1/models HTTP/1.1\r\nHost: x\r\nX-A: \x01\r\n\r\n",
            InvalidHeaderValue,
        ),
        (
            "GET /v1/models HTTP/1.1\r\nHost: x\r\nX-A: \u{e9}\r\n\r\n".as_bytes(),
            InvalidHeaderValue,
        ),
        (b"GET /v1/models HTTP/1.1\r\nX-A: b\r\n\r\n", MissingHost),
        (b"GET /v1/models HTTP/1.1\r\nHost: x\r\nhost: y\r\n\r\n", DuplicateHost),
        (b"POST /v1/responses HTTP/1.1\r\nHost: x\r\n\r\n", MissingContentLength),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: -1\r\n\r\n",
            InvalidContentLength,
        ),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: 1 2\r\n\r\n",
            InvalidContentLength,
        ),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: +5\r\n\r\n",
            InvalidContentLength,
        ),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: 0x10\r\n\r\n",
            InvalidContentLength,
        ),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999999999999999\r\n\r\n",
            InvalidContentLength,
        ),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: 33554433\r\n\r\n",
            BodyTooLarge,
        ),
        (
            b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nContent-Length: 0\r\n\r\n",
            Upgrade,
        ),
    ];
    for (bytes, refusal) in cases {
        assert_eq!(parse(bytes), Err(*refusal), "{}", String::from_utf8_lossy(bytes));
    }
    assert!(parse(b"POST /v1/responses HTTP/1.1\r\nHost: x\r\nContent-Length: 33554432\r\n\r\n").is_ok());
}

#[test]
fn request_smuggling_shapes_are_refused() {
    use Refusal::*;
    let cases: &[(&str, Refusal)] = &[
        // CL.CL: two lengths, equal or not.
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nContent-Length: 2\nContent-Length: 2\n\n",
            DuplicateContentLength,
        ),
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nContent-Length: 2\ncontent-length: 40\n\n",
            DuplicateContentLength,
        ),
        // CL.TE and TE.CL, in any order, case or value.
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nContent-Length: 2\nTransfer-Encoding: chunked\n\n",
            TransferEncodingWithLength,
        ),
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nTransfer-Encoding: chunked\nContent-Length: 2\n\n",
            TransferEncodingWithLength,
        ),
        (
            "POST /v1/responses HTTP/1.1\nHost: x\ntransfer-encoding: identity\n\n",
            TransferEncoding,
        ),
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nTRANSFER-ENCODING: gzip, chunked\n\n",
            TransferEncoding,
        ),
        // A folded continuation that would carry Transfer-Encoding or a length.
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nX-A: b\n Transfer-Encoding: chunked\nContent-Length: 2\n\n",
            ObsFold,
        ),
        ("POST /v1/responses HTTP/1.1\nHost: x\nContent-Length:\n 2\n\n", ObsFold),
        // Whitespace before the colon hides a header from some parsers.
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nTransfer-Encoding : chunked\nContent-Length: 2\n\n",
            WhitespaceBeforeColon,
        ),
        (
            "POST /v1/responses HTTP/1.1\nHost: x\nContent-Length\t: 2\n\n",
            WhitespaceBeforeColon,
        ),
    ];
    for (text, refusal) in cases {
        assert_eq!(parse(&head(text)), Err(*refusal), "{text}");
    }
    let rule = &ANTHROPIC;
    let key = placeholder(rule);
    let doubled = head(&format!(
        "POST /v1/messages HTTP/1.1\nHost: x\nx-api-key: {key}\nX-Api-Key: {key}\nContent-Length: 0\n\n"
    ));
    assert_eq!(planned(rule, &doubled), Err(DuplicateCredential));
    let doubled = head(&format!(
        "POST /v1/messages HTTP/1.1\nHost: x\nAuthorization: Bearer {key}\nauthorization: Bearer other\nContent-Length: 0\n\n"
    ));
    assert_eq!(planned(rule, &doubled), Err(DuplicateCredential));
}

#[test]
fn refusals_map_to_one_status_each() {
    use Refusal::*;
    let expected: &[(Refusal, u16)] = &[
        (HeadTooLarge, 431),
        (BodyTooLarge, 413),
        (MissingContentLength, 411),
        (TransferEncoding, 411),
        (TransferEncodingWithLength, 400),
        (DuplicateContentLength, 400),
        (ObsFold, 400),
        (AbsoluteForm, 400),
        (UnsupportedMethod, 403),
        (RouteNotAllowed, 403),
        (QueryNotAllowed, 403),
        (MissingCredential, 403),
        (WrongCredential, 403),
        (PlaceholderLeak, 400),
    ];
    for (refusal, code) in expected {
        assert_eq!(refusal.status().code(), *code, "{refusal:?}");
        let name = refusal.name();
        assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{name}");
    }
}

/// Every rule's allow-list: each listed route passes, its neighbours do not.
#[test]
fn each_backend_relays_exactly_its_allow_list() {
    let hits: &[(&InjectionRule, &str, &str)] = &[
        (&OPENAI, "POST", "/v1/responses"),
        (&OPENAI, "GET", "/v1/models"),
        (&OPENAI, "GET", "/v1/models?client_version=0.164.0"),
        (&ANTHROPIC, "POST", "/v1/messages"),
        (&ANTHROPIC, "POST", "/v1/messages?beta=true"),
        (&ANTHROPIC, "POST", "/v1/messages/count_tokens?beta=true"),
        (&ANTHROPIC, "GET", "/v1/models"),
        (&ANTHROPIC, "GET", "/v1/models?beta=true&limit=20"),
        (
            &GEMINI,
            "POST",
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
        ),
        (&GEMINI, "POST", "/v1beta/models/gemini-2.5-pro:streamGenerateContent"),
        (&GEMINI, "POST", "/v1beta/models/gemini-3-flash:generateContent"),
        (&GEMINI, "POST", "/v1beta/models/gemini-3-flash:countTokens"),
        (
            &GEMINI,
            "POST",
            "/v1beta/models/gemini-3-flash:generateContent?key=whatever",
        ),
        (&XAI, "POST", "/v1/responses"),
        (&XAI, "POST", "/v1/chat/completions"),
        (&XAI, "GET", "/v1/api-key"),
        (&XAI, "GET", "/v1/models"),
    ];
    for (rule, method, target) in hits {
        let outbound = planned(rule, &request(rule, method, target));
        assert!(
            outbound.is_ok(),
            "{} {method} {target}: {outbound:?}",
            rule.origin.host()
        );
    }
    use Refusal::{QueryNotAllowed as Query, RouteNotAllowed as Route};
    let misses: &[(&InjectionRule, &str, &str, Refusal)] = &[
        (&OPENAI, "GET", "/v1/responses", Route),
        (&OPENAI, "POST", "/v1/chat/completions", Route),
        (&OPENAI, "POST", "/v1/files", Route),
        (&OPENAI, "POST", "/v1/responses/x", Route),
        (&OPENAI, "POST", "/v1/responses?stream=true", Query),
        (&OPENAI, "GET", "/v1/models?client_version=1&client_version=2", Query),
        (&ANTHROPIC, "POST", "/v1/messages?beta=false", Query),
        (&ANTHROPIC, "POST", "/v1/messages/batches", Route),
        (&ANTHROPIC, "GET", "/v1/messages", Route),
        (&ANTHROPIC, "GET", "/v1/models?limit=ten", Query),
        (&ANTHROPIC, "POST", "/v1/complete", Route),
        (&GEMINI, "POST", "/v1beta/models/gemini:embedContent", Route),
        (&GEMINI, "POST", "/v1beta/models/:generateContent", Route),
        (&GEMINI, "POST", "/v1beta/models/a/b:generateContent", Route),
        (&GEMINI, "POST", "/v1beta/models/a@b:generateContent", Route),
        (&GEMINI, "GET", "/v1beta/models", Route),
        (&GEMINI, "POST", "/v1beta/tunedModels/x:generateContent", Route),
        (&GEMINI, "POST", "/v1beta/models/x:generateContent?alt=sse", Query),
        (
            &GEMINI,
            "POST",
            "/v1beta/models/x:streamGenerateContent?alt=json",
            Query,
        ),
        (&XAI, "POST", "/v1/messages", Route),
        (&XAI, "POST", "/v1/api-key", Route),
        (&XAI, "GET", "/v1/models?limit=1", Query),
        (&XAI, "POST", "/v1/images/generations", Route),
    ];
    for (rule, method, target, refusal) in misses {
        assert_eq!(
            planned(rule, &request(rule, method, target)),
            Err(*refusal),
            "{} {method} {target}",
            rule.origin.host()
        );
    }
}

#[test]
fn the_rule_table_matches_the_design() {
    let table = |rule: &InjectionRule| {
        rule.routes
            .iter()
            .map(|r| format!("{} {}", r.method.as_str(), r.path))
            .collect::<Vec<_>>()
    };
    assert_eq!(table(&OPENAI), ["POST /v1/responses", "GET /v1/models"]);
    assert_eq!(
        table(&ANTHROPIC),
        ["POST /v1/messages", "POST /v1/messages/count_tokens", "GET /v1/models"]
    );
    assert_eq!(
        table(&GEMINI),
        [
            "POST /v1beta/models/{model}:streamGenerateContent",
            "POST /v1beta/models/{model}:generateContent",
            "POST /v1beta/models/{model}:countTokens"
        ]
    );
    assert_eq!(
        table(&XAI),
        [
            "POST /v1/responses",
            "POST /v1/chat/completions",
            "GET /v1/api-key",
            "GET /v1/models"
        ]
    );
    fn set(rule: &InjectionRule) -> (&str, u16, &str, Scheme) {
        (rule.origin.host(), rule.origin.port(), rule.set.as_str(), rule.scheme)
    }
    assert_eq!(set(&OPENAI), ("api.openai.com", 443, "authorization", Scheme::Bearer));
    assert_eq!(set(&ANTHROPIC), ("api.anthropic.com", 443, "x-api-key", Scheme::Raw));
    assert_eq!(
        set(&GEMINI),
        ("generativelanguage.googleapis.com", 443, "x-goog-api-key", Scheme::Raw)
    );
    assert_eq!(set(&XAI), ("api.x.ai", 443, "authorization", Scheme::Bearer));
}

fn spliced(outbound: &Outbound, secret: &[u8]) -> String {
    String::from_utf8(outbound.splice(secret).unwrap().concat()).unwrap()
}

#[test]
fn the_outbound_head_is_rewritten_stripped_and_slotted() {
    let rule = &ANTHROPIC;
    let key = placeholder(rule);
    let inbound = head(&format!(
        "POST /v1/messages?beta=true HTTP/1.1\nHost: 127.0.0.1:3130\nAuthorization: Bearer {key}\nx-api-key: {key}\nanthropic-version: 2023-06-01\nProxy-Authorization: Basic eA==\nConnection: keep-alive\nKeep-Alive: timeout=5\nTE: trailers\nTrailer: X\nAccept: text/event-stream\nContent-Length: 00012\n\n"
    ));
    let outbound = planned(rule, &inbound).unwrap();
    assert_eq!(outbound.origin, rule.origin);
    assert_eq!(outbound.content_length, 12);
    assert_eq!(outbound.route.to_string(), "/v1/messages");
    assert_eq!(
        spliced(&outbound, b"sk-ant-REAL"),
        "POST /v1/messages?beta=true HTTP/1.1\r\nHost: api.anthropic.com\r\nanthropic-version: 2023-06-01\r\nAccept: text/event-stream\r\nContent-Length: 12\r\nConnection: close\r\nx-api-key: sk-ant-REAL\r\n\r\n"
    );
    let rule = &GEMINI;
    let inbound = request(
        rule,
        "POST",
        "/v1beta/models/m:streamGenerateContent?key=AIza-STRIPPED&alt=sse",
    );
    let text = spliced(&planned(rule, &inbound).unwrap(), b"AIza-REAL");
    assert!(
        text.starts_with(
            "POST /v1beta/models/m:streamGenerateContent?alt=sse HTTP/1.1\r\nHost: generativelanguage.googleapis.com\r\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("Connection: close\r\nx-goog-api-key: AIza-REAL\r\n\r\n"),
        "{text}"
    );
    assert!(!text.contains("STRIPPED"));
    let rule = &OPENAI;
    let text = spliced(
        &planned(rule, &request(rule, "GET", "/v1/models")).unwrap(),
        b"sk-proj-REAL",
    );
    assert_eq!(
        text,
        "GET /v1/models HTTP/1.1\r\nHost: api.openai.com\r\nConnection: close\r\nauthorization: Bearer sk-proj-REAL\r\n\r\n"
    );
}

#[test]
fn a_key_that_could_add_headers_is_never_spliced() {
    let outbound = planned(&XAI, &request(&XAI, "GET", "/v1/models")).unwrap();
    for bad in [&b""[..], b"xai-a\r\nX-Evil: 1", b"xai a", b"xai-\x7f", &[b'k'; 4097]] {
        assert_eq!(outbound.splice(bad), Err(InvalidSecret));
    }
    assert!(outbound.splice(b"xai-OK").is_ok());
}

#[test]
fn only_the_attempts_own_placeholder_in_every_credential_header_passes() {
    let rule = &ANTHROPIC;
    let other = AttemptCap::new(
        AttemptId("attempt-2".into()),
        "claude+work",
        Token::from_bytes(*b"another-token-16"),
    );
    let stale = other.placeholder(rule.placeholder).expose().to_owned();
    let key = placeholder(rule);
    let send = |credentials: &str| {
        planned(
            rule,
            &head(&format!(
                "POST /v1/messages HTTP/1.1\nHost: x\n{credentials}Content-Length: 0\n\n"
            )),
        )
        .map(drop)
    };
    assert_eq!(send(&format!("x-api-key: {key}\n")), Ok(()));
    assert_eq!(send(&format!("Authorization: Bearer {key}\n")), Ok(()));
    assert_eq!(
        send(&format!("authorization: bearer {key}\nX-API-KEY: {key}\n")),
        Ok(())
    );
    assert_eq!(send(""), Err(Refusal::MissingCredential));
    assert_eq!(send(&format!("x-api-key: {stale}\n")), Err(Refusal::WrongCredential));
    assert_eq!(
        send(&format!("x-api-key: {key}\nAuthorization: Bearer {stale}\n")),
        Err(Refusal::WrongCredential)
    );
    assert_eq!(send(&format!("Authorization: {key}\n")), Err(Refusal::WrongCredential));
    assert_eq!(
        send(&format!("Authorization: Basic {key}\n")),
        Err(Refusal::WrongCredential)
    );
    assert_eq!(
        send(&format!("x-api-key: {}\n", key.to_uppercase())),
        Err(Refusal::WrongCredential)
    );
    assert_eq!(send(&format!("x-api-key: {key}0\n")), Err(Refusal::WrongCredential));
    assert_eq!(
        send("x-api-key: sk-ant-api03-real-looking\n"),
        Err(Refusal::WrongCredential)
    );
    // Another shape's prefix is not this rule's placeholder.
    let openai = cap().placeholder(Shape::OpenAiProject).expose().to_owned();
    assert_eq!(send(&format!("x-api-key: {openai}\n")), Err(Refusal::WrongCredential));
    // A credential header the rule does not set still has to hold the placeholder.
    assert_eq!(
        send(&format!("x-api-key: {key}\nx-goog-api-key: other\n")),
        Err(Refusal::WrongCredential)
    );
}

#[test]
fn placeholders_have_each_clis_shape_and_parse_back() {
    let token = Token::from_bytes([0xab; 16]);
    let cap = AttemptCap::new(AttemptId("a".into()), (), token.clone());
    let openai = cap.placeholder(Shape::OpenAiProject);
    assert_eq!(openai.expose(), format!("sk-proj-ctl-{}", "ab".repeat(16)));
    assert_eq!(
        cap.placeholder(Shape::Plain).expose(),
        format!("ctl-{}", "ab".repeat(16))
    );
    assert_eq!(Placeholder::parse(openai.expose(), Shape::OpenAiProject), Some(token));
    assert_eq!(Placeholder::parse(openai.expose(), Shape::Plain), None);
    assert_eq!(Placeholder::parse("ctl-AB", Shape::Plain), None);
    assert!(!format!("{openai:?}{cap:?}").contains("abab"));
}

#[test]
fn no_outbound_head_ever_contains_the_inbound_placeholder() {
    let decorations = [
        "",
        "Proxy-Authorization: Bearer {key}\n",
        "User-Agent: cli/1.0\nAccept: */*\n",
        "x-goog-api-key: {key}\n",
        "X-Echo: {key}\n",
        "X-Echo: {KEY}\n",
        "Cookie: k={key}\n",
    ];
    let targets = [
        "/v1/responses",
        "/v1/models",
        "/v1/messages",
        "/v1beta/models/m:generateContent?key={key}",
        "/v1/models?client_version={key}",
        "/v1/{key}",
    ];
    let mut relayed = 0;
    for rule in [&OPENAI, &ANTHROPIC, &GEMINI, &XAI] {
        let key = placeholder(rule);
        let fill = |text: &str| text.replace("{key}", &key).replace("{KEY}", &key.to_uppercase());
        let credentials = [
            format!("Authorization: Bearer {key}\n"),
            format!("x-api-key: {key}\n"),
            format!("x-goog-api-key: {key}\n"),
            format!("Authorization: Bearer {key}\nx-api-key: {key}\n"),
        ];
        let credentials = &credentials;
        for (target, decoration, credential, method) in targets.iter().flat_map(|t| {
            decorations.iter().flat_map(move |d| {
                credentials
                    .iter()
                    .flat_map(move |c| ["GET", "POST"].map(move |m| (t, d, c, m)))
            })
        }) {
            let inbound = head(&format!(
                "{method} {} HTTP/1.1\nHost: x\n{credential}{}Content-Length: 0\n\n",
                fill(target),
                fill(decoration)
            ));
            if let Ok(outbound) = planned(rule, &inbound) {
                relayed += 1;
                let sent = spliced(&outbound, b"REAL-KEY").to_ascii_lowercase();
                assert!(!sent.contains(&key[key.len() - 32..]), "{sent}");
            }
        }
    }
    assert!(relayed > 40, "the property ran over relayed requests: {relayed}");
}

#[test]
fn the_budget_bounds_streams_rate_and_bytes_and_returns_decisions_as_data() {
    let limits = Limits {
        streams: 2,
        per_minute: 3,
        total_bytes: 1000,
    };
    let start = Budget::new(limits);
    let (one, a) = start.admit(Millis(0), 100).unwrap();
    assert_eq!(a.allowance(), 900);
    let (two, b) = one.admit(Millis(1), 100).unwrap();
    assert_eq!(two.admit(Millis(2), 1), Err(Deny::Streams));
    let freed = two.release(a, 50);
    let (three, c) = freed.admit(Millis(3), 100).unwrap();
    assert_eq!(c.allowance(), 1000 - 350);
    let calm = three.release(b, 0).release(c, 0);
    assert_eq!(
        calm.admit(Millis(59_999), 1),
        Err(Deny::Rate),
        "three started this minute"
    );
    let (_, d) = calm.admit(Millis(60_003), 1).unwrap();
    assert_eq!(d.allowance(), 1000 - 351);
    assert_eq!(
        calm.admit(Millis(60_003), 700),
        Err(Deny::Bytes),
        "350 used + 700 > 1000"
    );
    assert_eq!(Limits::DEFAULT.streams, 8);
    assert_eq!(
        [Deny::Streams, Deny::Rate, Deny::Bytes].map(Deny::name),
        ["too_many_streams", "rate_limited", "byte_budget_spent"]
    );
}

#[test]
fn retention_drops_old_unreadable_then_oldest_lines_until_the_log_fits() {
    const DAY: u64 = 24 * 60 * 60;
    let now = 100 * DAY;
    let line = |time: Option<u64>, bytes| Line { time, bytes };
    let retention = Retention {
        max_age_seconds: 30 * DAY,
        max_bytes: 1000,
    };
    let cases: &[(&[Line], u64, &[usize])] = &[
        (&[], 100, &[]),
        (&[line(Some(now), 100), line(Some(now), 100)], 100, &[]),
        // Older than 30 days, or unreadable.
        (
            &[
                line(Some(now - 31 * DAY), 10),
                line(None, 10),
                line(Some(now - 29 * DAY), 10),
            ],
            10,
            &[0, 1],
        ),
        (&[line(Some(now - 30 * DAY), 10)], 10, &[]),
        // Over the size cap: the oldest fresh lines go until the new one fits.
        (
            &[
                line(Some(now - 5), 400),
                line(Some(now - 4), 400),
                line(Some(now - 3), 150),
            ],
            100,
            &[0],
        ),
        (
            &[
                line(Some(now - 5), 400),
                line(Some(now - 4), 400),
                line(Some(now - 3), 200),
            ],
            500,
            &[0, 1],
        ),
        (
            &[line(Some(now - 40 * DAY), 900), line(Some(now - 4), 900)],
            200,
            &[0, 1],
        ),
    ];
    for (lines, incoming, dropped) in cases {
        assert_eq!(
            prune(lines, now, *incoming, retention),
            *dropped,
            "{lines:?} + {incoming}"
        );
    }
    assert_eq!(Retention::DEFAULT.max_age_seconds, 30 * DAY);
}

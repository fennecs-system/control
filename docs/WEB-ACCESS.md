# Scoped pages and offline browser evidence

Workers and persistent services remain offline. The HTTP broker fetches a page
on behalf of one chat after the local operator approves an exact origin.

1. Open **Tools → Browser → Web access**.
2. Select the chat, enter an origin such as `https://example.com`, and choose
   a lease (default 15 minutes, maximum one hour).
3. Ask that chat to use `control_web_fetch` with a URL on that origin.
4. Revoke the grant on the same page when finished. Redirects to another origin
   require a separate grant. Scheme, hostname and port must all match.

Grants belong to the chat's captured actor and persist across Control restarts
without extending their expiry. They authorize the HTTP page broker only: they
are not a container network grant, general proxy, authenticated browser session,
or permission to access local services. Agents cannot create or extend grants.
A delegated chat needs its own approval; it does not inherit another chat's grant.
The operator page is behind the existing authenticated local website gate.

## Tools

`control_web_fetch {"url":"https://example.com/page"}` returns the final URL,
status, title, up to 16,000 characters of normalized untrusted text, and up to 20
links. Script/style/template text is excluded. This is source evidence, not
instructions. Optional `"capture":true` renders the fetched page in offline
Firefox and returns a PNG artifact link. Remote assets, cookies, authentication
and additional page requests are not loaded. UTF-8 HTML/XHTML and plain text are
supported; other encodings and content types fail with a bounded error.

`control_browser_capture {"html":"<!doctype html>...","width":1024,"height":768}`
lets a worker verify a small generated page without any web grant. Supply at most
16,000 UTF-8 bytes of self-contained HTML with inline CSS/assets. It returns an
artifact link and an inline MCP image when the PNG is at most 256 KiB. Larger
images remain viewable in the authenticated website. Viewports are 240–2560
pixels per side. The existing operator `control.browser.capture` file tool and
`control browser capture` CLI remain available.

This supports the basic generated-page demonstration: build a self-contained
page, capture it, inspect the screenshot, and share the preview/artifact. It does
not claim to test a running preview's HTTP behavior, load remote assets, navigate
interactively, or provide a general browser automation harness. Those need their
own scoped capability. A complex page can still use the existing workspace and
preview tools; this capture tool's inline input is deliberately small.

## Enforced boundary and limits

- Every fetch checks the current active attempt, actor and chat grant before DNS,
  after DNS, after each response, and before returning evidence. Revocation stops
  new requests and suppresses a response which completed after revocation; it
  cannot undo bytes already sent. An in-flight request has a three-second timeout.
- Every redirect is parsed and checked again. At most four redirects are followed.
  Each hop resolves afresh and requires every answer to be public unicast;
  loopback, LAN, metadata, multicast, documentation, mapped and transition IP
  ranges are rejected. Mixed public/private answers fail closed.
- The verified address set is pinned into that request's HTTP client. Automatic
  redirects and environment proxies are disabled. TLS still verifies the original
  hostname; neither a credential store nor a host browser profile is used.
- GET only; no custom headers, request body, cookie jar or authorization forwarding.
  Never put secrets into URLs or into queries sent to public sites.
- Up to 64 active grants, four concurrent fetches, and four outstanding resolver
  threads. Resolver waits are bounded to two seconds; a timed-out OS lookup keeps
  its slot until it ends. Fetch budget is eight seconds, plus at most a resolver
  wait before its deadline is observed. HTTP bodies are limited to 1 MiB.
- Captures use the existing pinned Nix Firefox image: network disabled, read-only,
  non-root, capabilities dropped, bounded CPU/memory/PIDs/output and private tmpfs.
  Only a private copy of the supplied HTML is mounted read-only. No worker tree,
  host profile or credentials are mounted. Worker captures have a 20-second
  deadline and use the existing 64-artifact / 248 MiB browser quota.
- No new crate dependencies, host service, container bridge or Docker socket is
  exposed to models. HTTP transfer does not hold the control-plane state lock.

Minecraft still needs separately enforced TCP destination/port/lifetime access
for persistent tools. This HTTP capability does not implement that transport.

## Local verification

```sh
nix develop --command cargo test --locked --bin control web_access
nix develop --command cargo clippy --locked --workspace --all-targets -- -D warnings
```

The normal tests cover approval persistence and failed-save rollback, chat/actor
separation, scheme/port boundaries, expiry/clock rollback, revocation during DNS
and response handling, redirect escape, DNS rebinding and mixed answers, evidence
limits and an actual HTTP client against an isolated TCP fixture. Only the test
transport receives a loopback address; production rejects it before connecting.

The ignored `web_access::integration_tests::live_worker_capture_returns_viewable_offline_artifact`
check needs `CONTROL_BROWSER_IMAGE` set to an existing pinned Nix Firefox image,
plus isolated `CONTROL_CONFIG_DIR` and `CONTROL_STATE_DIR` directories. It makes
no network requests or model calls. On the development laptop the 640×480 fixture
produced a 15,491-byte PNG in 1.46 seconds; this is a single local observation,
not a throughput benchmark or performance promise.

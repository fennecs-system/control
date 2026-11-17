# Scoped pages and offline browser evidence

Workers and persistent services stay offline. When the local operator approves an
exact origin for one chat, Control's HTTP broker fetches pages on that chat's
behalf.

1. Open **Tools → Browser → Web access** (`/tools/browser`).
2. Select the chat, enter an origin such as `https://example.com`, and choose a
   lease (default 15 minutes, maximum one hour).
3. Ask that chat to use `control_web_fetch` with a URL on that origin.
4. Revoke the grant on the same page when finished.

Scheme, host and port must all match; a redirect to another origin needs its own
grant. Grants belong to the chat's captured actor and survive restarts without
extending their expiry. Agents cannot create or extend grants, and a delegated
chat needs its own. A grant authorizes only the page broker: it is not a
container network grant, a general proxy, an authenticated browser session or
access to local services. Non-HTTP ingress and egress use separate
[scoped TCP ports](PORTS.md).

## Tools

`control_web_fetch {"url":"https://example.com/page"}` returns the final URL,
status, title, up to 16,000 characters of normalized text and up to 20 links.
Script, style and template text is dropped. The result is untrusted evidence, not
instructions. `"capture":true` also renders the fetched page in offline Firefox
and returns a PNG artifact link; remote assets, cookies and further requests are
not loaded.

- Only UTF-8 HTML/XHTML and plain text are supported; anything else fails with a
  bounded error.
- URL fragments are stripped; URLs with credentials are denied.
- Pages over 1 MiB return their first 1 MiB. `body_truncated`, `text_truncated`
  and `truncated` flag partial evidence, which (like its screenshot) does not
  represent the whole page. A character split at the limit is dropped; malformed
  UTF-8 inside the kept prefix is an error.

`control_browser_capture {"html":"<!doctype html>...","width":1024,"height":768}`
lets a worker check a small generated page without any grant. It takes up to
16,000 bytes of self-contained HTML (inline CSS and assets) and a viewport of
240–2560 pixels per side. It returns an artifact link, plus an inline image when
the PNG is at most 256 KiB; larger images open in the website. The operator
`control.browser.capture` tool and `control browser capture` CLI also exist.

Capture supports the basic [demonstration](DEMONSTRATIONS.md): build a page,
capture it, inspect the screenshot and share the artifact. It does not test a
running preview over HTTP, load remote assets, navigate interactively or
automate a browser. Larger pages can use the workspace and preview tools.

## Enforced boundary and limits

- Each fetch checks the active attempt, actor and chat grant before DNS, after
  DNS, after each response and before returning evidence. Revocation stops new
  requests and suppresses responses that finish afterwards; it cannot recall
  bytes already sent.
- Redirects are followed manually, at most four, each rechecked against the
  grants. Each hop resolves afresh and every answer must be public unicast:
  loopback, private, link-local, metadata, multicast, documentation, mapped and
  transition ranges are rejected, and mixed public/private answers fail closed.
- The verified addresses are pinned into that request's client. Environment
  proxies are disabled. TLS verifies the original hostname. No credential store
  or host browser profile is used.
- GET only: no custom headers, body, cookies or forwarded authorization. Never
  put secrets in URLs or in queries to public sites.
- At most 64 active grants, four concurrent fetches and four outstanding resolver
  threads. Resolution waits up to two seconds (a timed-out OS lookup holds its
  slot until it returns). Each fetch has an eight-second budget and three-second
  I/O timeouts. The broker reads at most 1 MiB plus one probe byte, then drops
  the connection. Busy fetches fail at once with a retry-later error; there is no
  hidden queue.
- HTTP slots are released before parsing and capture. Captures have their own
  lock and quota, and the grant is checked again before evidence is returned.
- Captures run in the pinned Nix Firefox image: no network, read-only, non-root,
  capabilities dropped, bounded CPU/memory/PIDs/output and a private tmpfs. Only a
  private read-only copy of the HTML is mounted; no worker tree, host profile or
  credentials. Worker captures have a 20-second deadline and share the browser
  quota of 64 artifacts / 248 MiB.
- Models see no host service, container bridge or Docker socket. Transfers do not
  hold the control-plane state lock.

## Tests

```sh
nix develop --command cargo test --locked --bin control web_access
```

These cover grant persistence and save-failure rollback, chat/actor separation,
scheme/port matching, expiry and clock rollback, revocation during DNS and
response handling, redirect escape, DNS rebinding and mixed answers, evidence
limits, and a real HTTP client against a local TCP fixture. Only the test
transport accepts a loopback address.

The ignored `web_access::integration_tests::live_worker_capture_returns_viewable_offline_artifact`
check needs `CONTROL_BROWSER_IMAGE` set to a pinned Firefox image and scratch
`CONTROL_CONFIG_DIR`/`CONTROL_STATE_DIR`. It makes no network requests or model
calls.

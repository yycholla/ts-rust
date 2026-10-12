# Fetch constructor integration

Native implementation target: Fetch/Web IDL algorithms with an explicit server
Headers profile, matching installed Undici 7.23.0 where it intentionally differs
from browser filtering. No global fetch until a real host transport exists.

- Request converts RequestInfo first, then RequestInit members in lexicographic
  order: body, cache, credentials, duplex, headers, integrity, keepalive, method,
  mode, priority, redirect, referrer, referrerPolicy, signal, window. Dictionary
  property reads include inherited getters. URL parsing and request initialization
  follow conversion; body extraction follows header filling and method checks.
  An inherited body uses the identity proxy, not tee.
- Response converts BodyInit first, then headers, status, statusText. It extracts
  the body before initialization validates status/statusText, fills headers, and
  rejects a body for null-body statuses.
- Response.json converts init before JSON serialization. Use captured ECMAScript
  intrinsics, not the runtime's strict JSON transport or serde_json serialization.
- Response.redirect converts URL and unsigned-short status, parses the URL before
  validating the redirect status, and creates immutable headers.
- HeadersInit conversion retains full raw nested sequences and ByteStrings.
  Arity and HTTP validation belong to later fill; moving them into dictionary
  conversion changes getter and exception ordering.
- Body consumption distinguishes null from an empty stream; null can be consumed
  repeatedly. BodyUsed tests disturbance, while unusability tests both disturbance
  and locking. Fetch full reads retain their reader and do not auto-cancel.
- Clone tees a body and replaces its original stream with branch one. A Request
  clone receives a distinct native dependent signal. Server headers retain cookies;
  browser forbidden-header filtering requires a separate explicit profile.

Kit uses existing objects as dictionaries (`new Response(null, response)`,
`new Response(response.body, response)`, and `new Request(url, request)`). Main
callers: runtime/server/fetch.js, page/load_data.js, endpoint.js, respond.js and
Kit 3 remote-functions.js. Remote live queries also require actual timers.

Sources: [Fetch](https://fetch.spec.whatwg.org/#request-class),
[Web IDL dictionaries](https://webidl.spec.whatwg.org/#es-dictionary), and installed
Undici 7.23.0 `lib/web/webidl/index.js` and `lib/web/fetch/{request,response,headers}.js`.

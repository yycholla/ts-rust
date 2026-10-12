# Native SvelteKit runtime

`ts_svelte_runtime` owns SvelteKit's server initialization and postbuild execution.
It uses the separate `ts_js_runtime` ECMAScript engine crate. The component
compiler, Kit generators, bundler, and filesystem build host remain separate.

The input is an explicit `BootstrapRequest`: the emitted server build identity,
ESM sources and exact import resolutions, generated manifest entry, initialization
profile, environment values, and server-relative asset bytes. The build host's
`execution_snapshot::prepare` constructs this input from actual emitted artifacts.
It does not execute configuration files or resolve packages during execution.

Initialization evaluates the actual generated manifest, preserving its Sets,
functions, lazy imports, and module identity. Kit 2 initializes `internal.js` and
`env.js`; Kit 3 awaits `index.js`'s `configure` export. Application initialization
hooks and route handlers are not invoked by these operations. Native `read`
returns readable byte streams from the supplied asset snapshot.

`load_nodes` starts all manifest node loaders before awaiting their combined
result, retains their engine objects, and reports load presence separately from
callability. Repeated calls reuse the loaded modules. Execution errors poison the
session; dispose it before retrying with corrected input.

The `svelte-runtime --serve` CLI accepts newline-delimited JSON. Its operations
are `capabilities`, `create { bootstrap, fetchOptions?, cryptoOptions? }`,
`loadNodes { session }`, `analyzeMetadata { session, options }`, and
`dispose { session }`. Responses use
`{ ok, result }` or `{ ok: false, error: { code, message } }`. The same persistent
service is exported to WASM as `svelte_runtime_json`. At most eight sessions are
retained; request, module, asset, instruction, and job limits apply.

Environment and entropy come from the caller. There is no implicit network,
filesystem, process environment, or timer capability. `analyzeMetadata` evaluates actual route modules and awaited entries for the
registered Kit versions, retaining their native values. Its JSON view tags absent,
undefined and JSON values explicitly. Options select an exact registered version
(`profile`) and optional hash routing (`hash`); they cannot change once analysis
is cached. `analyzeRemotes { session, chunks }` follows route analysis and preserves actual
remote export metadata and loader order; it never invokes remote handlers.
Adapter feature checks and prerender execution remain incomplete operations.
This crate does not establish complete SvelteKit parity.

Prerender visits use a fresh rendering session: `createPrerender { input,
fetchOptions?, cryptoOptions? }` configures the server, `startPrerender { session }`
invokes its initialization hooks, and `visitPrerender { session, path }` dispatches
an actual GET request. Metadata and rendering sessions share the eight-session
limit and `dispose`, but cannot be used interchangeably. Configuration and start
are separate so a build can determine whether prerender work exists before
running application hooks.

Visits return response bytes, raw byte headers/status text, decoded route IDs and
ordered dependency responses. The driver consumes the main body before iterating
the live dependency map, preserves binary subviews, and keeps remote response
maps across visits. `publishPrerender { session, file, bytes }` explicitly makes
caller-produced files available to subsequent reads. Responses are not saved
automatically. Crawling, redirect/error policy, remote prerender input expansion,
final artifact naming and adapter emulation remain separate build operations;
`prerenderVisits` therefore does not imply full `prerender` capability.

The build host can preserve policy and save ordering with staged visits:
`beginPrerenderVisit { session, path }` returns response headers and a visit
token; `readPrerenderMain { session, token }` consumes the main body.
After saving and publishing that response, `nextPrerenderDependency { session,
token }` inspects the next dependency without reading its body getter or stream.
`readPrerenderDependency { session, token }` consumes that dependency. Repeat until
the next dependency is `null`, which releases the visit. Dependencies added
during consumption remain visible. Each rendering session currently permits one
active staged visit; protocol misuse poisons it. The convenience `visitPrerender`
collects all stages without intervening policy callbacks or publication.

The `prerender_remotes` Rust helper separates remote discovery before hooks from
input evaluation after hooks. It retains real JavaScript arguments and accepts
the actual snapshot's Kit serializer function, preserving Kit 2 transport hooks
and Kit 3 input-error formatting. Its enumeration API does not yet schedule
visits concurrently with input evaluation.

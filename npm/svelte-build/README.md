# svelte-build (native SvelteKit build prototype)

A Rust build of a SvelteKit app that writes the same `build/` as `vite build` with
`@sveltejs/adapter-node`, without running Vite or Node. It is a prototype of the
[ts-rust fork](https://github.com/yycholla/ts-rust/tree/svelte-prototype/crates/ts_svelte_build).
It is not part of Svelte or SvelteKit.

Linux x64 only, glibc 2.28 or later. `BUILD.txt` names the source commit and the binary's sha256.

## Support

- SvelteKit 2 with `@sveltejs/adapter-node` 5.5.4, and SvelteKit 3 with adapter-node 6.0.0.
- The project is read statically: `svelte.config.js` (Kit 2) or the `sveltekit({...})` options in
  `vite.config` (Kit 3). What it cannot read or does not support is reported as a requirement, and
  the build stops unless the request accepts it.
- Prerendering is not run yet: a project that prerenders pages does not complete.

## Use

The binary reads one JSON request on stdin and writes one JSON response line on stdout.

```sh
npm install --no-save @ts-rust/svelte-build
echo '{"op":"project","project":{"root":"'"$PWD"'","environment":{"NODE_ENV":"production"}},"outputDirectory":"'"$PWD"'/native-out"}' \
  | npx svelte-build
node native-out/build/index.js
```

- `{"op":"capabilities"}` lists the supported versions and pipeline stages.
- `"requestOnly": true` reports what the loader read and its requirements without building.
- `"acceptRequirements": ["vite/sentry", ...]` accepts named requirements (for example a Vite
  plugin whose output the build does not reproduce).
- `project.define` and `project.environment` set Vite `define` values and the build environment.

The response's `ok` and `report.status` (`complete` when every phase finished) give the result.

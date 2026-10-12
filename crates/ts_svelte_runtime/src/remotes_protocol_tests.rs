//! JSON-boundary integration for actual native framework module execution.
use crate::protocol::Service;
use serde_json::{Value, json};

fn call(service: &mut Service, request: Value) -> Value {
    serde_json::from_str(&service.request_json(&request.to_string())).unwrap()
}

#[test]
fn json_remote_analysis_requires_routes_then_caches_real_wrappers_and_disposes() {
    let server = r#"
        import { state } from './state.js';
        export async function configure() {
            if (state.phase !== 'new') throw Error('configure repeated');
            await 0; state.phase = 'configured';
        }
        export const manifest = {
            nodes: [async () => {
                if (state.phase !== 'configured' || ++state.nodes !== 1) throw Error('nodes out of order');
                await 0;
                return { index: 0, universal: {
                    entries: async () => {
                        if (++state.entries !== 1) throw Error('entries repeated');
                        await 0; state.phase = 'routes-complete'; return [{ slug: 'native' }];
                    }
                }};
            }],
            routes: [{ id: '/[slug]', page: { layouts: [], leaf: 0 } }],
            remotes: { hash: async function () {
                if (this !== undefined) throw Error('remote receiver bound');
                if (state.phase !== 'routes-complete' || ++state.remotes !== 1) throw Error('remote out of order or repeated');
                await 0; return import('./wrapper.js');
            }}
        };
    "#;
    let functions = r#"
        import { state } from './state.js';
        if (state.phase !== 'routes-complete') throw Error('remote module initialized too early');
        function query() { throw Error('analysis must not execute handler'); }
        Object.defineProperty(query, '__', { value: { type: 'query', id: 'hash/query', name: 'query' }});
        function prerender() { throw Error('analysis must not prerender'); }
        Object.defineProperty(prerender, '__', { value: {
            type: 'prerender', id: 'hash/prerender', name: 'prerender',
            inputs() { throw Error('analysis must not execute inputs'); }
        }});
        export { query, prerender };
    "#;
    // The wrapper is the actual Kit transform's namespace-forwarding shape.
    let wrapper = "import * as m from './functions.js'; export default m;";
    let bootstrap = json!({
        "serverBuildId":"remote-protocol-build",
        "snapshot": {
            "modules": {
                "server":{"source":server,"importMetaUrl":"file:///server.js"},
                "state":{"source":"export const state = { phase: 'new', nodes: 0, entries: 0, remotes: 0 };","importMetaUrl":"file:///state.js"},
                "wrapper":{"source":wrapper,"importMetaUrl":"file:///wrapper.js"},
                "functions":{"source":functions,"importMetaUrl":"file:///functions.js"}
            },
            "resolutions": {
                "server": {
                    "./state.js":{"kind":"module","id":"state"},
                    "./wrapper.js":{"kind":"module","id":"wrapper"}
                },
                "wrapper":{"./functions.js":{"kind":"module","id":"functions"}},
                "functions":{"./state.js":{"kind":"module","id":"state"}}
            }
        },
        "manifestEntry":"server",
        "initialization":{"profile":"kit3","server":"server"},
        "environment":{},"assets":{}
    });
    let mut service = Service::default();
    let created = call(&mut service, json!({"op":"create","bootstrap":bootstrap}));
    assert_eq!(created["ok"], true, "{created}");
    let session = created["result"]["session"].clone();
    let chunks = json!([{"hash":"hash","file":"src/functions.remote.ts"}]);
    let request = json!({"op":"analyzeRemotes","session":session,"chunks":chunks});
    let premature = call(&mut service, request.clone());
    assert_eq!(premature["ok"], false);
    assert_eq!(premature["error"]["code"], "kit_runtime_input");
    assert!(
        premature["error"]["message"]
            .as_str()
            .unwrap()
            .contains("route metadata before remote")
    );

    let route_request =
        json!({"op":"analyzeMetadata","session":session,"options":{"profile":"3.0.1"}});
    let routes = call(&mut service, route_request.clone());
    assert_eq!(routes["ok"], true, "{routes}");
    assert_eq!(
        routes["result"]["routes"][0]["entries"],
        json!({"kind":"json","value":["/native"]})
    );

    let result = call(&mut service, request.clone());
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["serverBuildId"], "remote-protocol-build");
    assert_eq!(
        result["result"]["remotes"],
        json!([{
            "hash":"hash","file":"src/functions.remote.ts",
            "exports":[
                {"name":"prerender","kind":{"kind":"json","value":"prerender"},"dynamic":{"kind":"undefined"}},
                {"name":"query","kind":{"kind":"json","value":"query"},"dynamic":{"kind":"json","value":true}}
            ]
        }])
    );
    assert_eq!(call(&mut service, request.clone()), result);
    assert_eq!(call(&mut service, route_request), routes);

    let changed = call(
        &mut service,
        json!({"op":"analyzeRemotes","session":session,"chunks":[]}),
    );
    assert_eq!(changed["error"]["code"], "kit_runtime_input");
    assert!(
        changed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("inventory cannot change")
    );
    // A preflight request error does not poison the initialized session/cache.
    assert_eq!(call(&mut service, request.clone()), result);
    assert_eq!(
        call(&mut service, json!({"op":"dispose","session":session}))["result"]["disposed"],
        true
    );
    assert_eq!(
        call(&mut service, request)["error"]["code"],
        "unknown_session"
    );
    assert_eq!(
        call(&mut service, json!({"op":"dispose","session":session}))["result"]["disposed"],
        false
    );
}

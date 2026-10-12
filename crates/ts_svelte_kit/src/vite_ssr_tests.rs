use super::*;
use serde_json::json;

fn policy(external: &[&str], no_external: &[&str]) -> SsrExternalPolicy {
    SsrExternalPolicy {
        external: external.iter().map(|s| (*s).into()).collect(),
        no_external: no_external
            .iter()
            .map(|s| NoExternal::Name((*s).into()))
            .collect(),
    }
}
fn resolved(path: &str) -> impl FnOnce(&str) -> Probe + '_ {
    move |_| Probe::Resolved(PathBuf::from(path))
}

#[test]
fn package_names_and_bare_imports_follow_vite() {
    assert_eq!(package_name("pg").as_deref(), Some("pg"));
    assert_eq!(
        package_name("@aws-sdk/client-s3/x").as_deref(),
        Some("@aws-sdk/client-s3")
    );
    assert_eq!(package_name("@scope"), None);
    assert!(is_bare_import("pg"));
    assert!(is_bare_import("@sentry/sveltekit"));
    assert!(!is_bare_import("$env/static/private"));
    assert!(!is_bare_import("./x"));
    assert!(!is_bare_import("C:/x"));
    assert!(!is_bare_import("a://b"));
}

#[test]
fn decisions_follow_create_is_external_order() {
    let p = policy(&["jsonwebtoken", "crypto"], &["svelte", "bits-ui"]);
    let filter = NoExternalFilter::new(&p).unwrap();
    let never = |_: &str| -> Probe { panic!("probe must not run") };
    assert!(decide(&p, &filter, "node:fs", never));
    assert!(decide(&p, &filter, "fs/promises", never));
    assert!(decide(&p, &filter, "jsonwebtoken", never));
    assert!(!decide(&p, &filter, "./local", never));
    assert!(!decide(&p, &filter, "bits-ui", never));
    assert!(!decide(&p, &filter, "svelte/internal", |_| {
        Probe::Unresolved
    }));
    // Ordinary packages: externalized only for JS files inside node_modules.
    assert!(decide(
        &p,
        &filter,
        "pg",
        resolved("/app/node_modules/pg/lib/index.js")
    ));
    assert!(decide(
        &p,
        &filter,
        "canvas",
        resolved("/app/node_modules/canvas/index")
    ));
    assert!(!decide(
        &p,
        &filter,
        "pdfjs-dist/web/pdf_viewer.css",
        resolved("/app/node_modules/pdfjs-dist/web/pdf_viewer.css")
    ));
    assert!(!decide(
        &p,
        &filter,
        "linked",
        resolved("/work/linked/index.js")
    ));
    assert!(!decide(&p, &filter, "missing", |_| Probe::Unresolved));
    // Configured package externals accept linked packages.
    let configured = policy(&["linked"], &[]);
    let filter = NoExternalFilter::new(&configured).unwrap();
    assert!(decide(
        &configured,
        &filter,
        "linked/sub",
        resolved("/work/linked/sub.js")
    ));
}

#[test]
fn kit_policy_adds_framework_entries_and_respects_user_externals() {
    let root = std::env::temp_dir().join(format!("ts-svelte-vite-ssr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let write = |path: &str, value: serde_json::Value| {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, value.to_string()).unwrap();
    };
    write(
        "package.json",
        json!({"dependencies":{"lib-svelte":"1","plain":"1","@types/x":"1"},"devDependencies":{"semi":"1"}}),
    );
    write(
        "node_modules/lib-svelte/package.json",
        json!({"name":"lib-svelte","exports":{".":{"svelte":"./x.svelte"}},"dependencies":{"inner":"1"}}),
    );
    write(
        "node_modules/inner/package.json",
        json!({"name":"inner","svelte":"./a.svelte"}),
    );
    write(
        "node_modules/plain/package.json",
        json!({"name":"plain","main":"index.js"}),
    );
    write(
        "node_modules/semi/package.json",
        json!({"name":"semi","peerDependencies":{"svelte":"5"}}),
    );
    let policy = kit2_server_policy(&root, &["semi".into()], &[]).unwrap();
    let names: Vec<_> = policy
        .no_external
        .iter()
        .map(|entry| match entry {
            NoExternal::Name(name) => name.clone(),
            NoExternal::Pattern(pattern) => format!("/{pattern}/"),
        })
        .collect();
    assert_eq!(
        names,
        [
            "esm-env",
            "@sveltejs/kit/src/runtime",
            "svelte",
            "/^svelte\\//",
            "esm-env",
            "lib-svelte",
            "inner"
        ]
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// vite-plugin-svelte 6.2.4 `buildExtraViteConfig` on the same installed project.
#[test]
#[ignore = "requires TS_SVELTE_BUILD_KIT_FIXTURE (installed vite-plugin-svelte 6.2.4 project) and node"]
fn crawl_matches_installed_vite_plugin_svelte() {
    let root = PathBuf::from(std::env::var_os("TS_SVELTE_BUILD_KIT_FIXTURE").unwrap());
    let plugin = root.join("node_modules/@sveltejs/vite-plugin-svelte");
    let version: Value =
        serde_json::from_slice(&std::fs::read(plugin.join("package.json")).unwrap()).unwrap();
    assert_eq!(version["version"], "6.2.4");
    let node = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("node"))
                .find(|candidate| candidate.is_file())
        })
        .expect("node on PATH");
    let module = url::Url::from_file_path(plugin.join("src/utils/options.js")).unwrap();
    let user_external = ["jsonwebtoken", "crypto"];
    let script = format!(
        "import {{buildExtraViteConfig}} from {module:?};\
         const c=await buildExtraViteConfig({{root:{root:?},isBuild:true,experimental:{{}}}},{{ssr:{{external:{external}}}}});\
         process.stdout.write(JSON.stringify({{noExternal:c.ssr.noExternal.map(v=>v instanceof RegExp?'/'+v.source+'/':v),external:c.ssr.external}}));",
        module = module.as_str(),
        root = root.to_str().unwrap(),
        external = serde_json::to_string(&user_external).unwrap(),
    );
    let output = std::process::Command::new(node)
        .args(["--input-type=module", "-e", &script])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: Value = serde_json::from_slice(&output.stdout).unwrap();
    let user_external: Vec<String> = user_external.iter().map(|s| (*s).into()).collect();
    let native = kit2_server_policy(&root, &user_external, &[]).unwrap();
    // Kit contributes its two entries ahead of the plugin's in this policy.
    let native: BTreeSet<String> = native.no_external[2..]
        .iter()
        .map(|entry| match entry {
            NoExternal::Name(name) => name.clone(),
            NoExternal::Pattern(pattern) => format!("/{pattern}/"),
        })
        .collect();
    let oracle_set: BTreeSet<String> = oracle["noExternal"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(native, oracle_set);
    assert_eq!(oracle["external"], json!([]));
}

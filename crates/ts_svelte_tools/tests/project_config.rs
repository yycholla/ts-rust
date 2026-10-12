#![cfg(not(target_arch = "wasm32"))]
use serde_json::json;
use ts_svelte_tools::project_check::Config;
#[test]
fn explicit_project_rejects_ambiguous_paths_and_settings() {
    let root = std::env::temp_dir().join(format!("svelte-config-test-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let deps = root.join("deps");
    std::fs::create_dir_all(deps.join("svelte")).unwrap();
    std::fs::write(deps.join("svelte/package.json"), r#"{"version":"5.53.11"}"#).unwrap();
    std::fs::write(root.join("A.svelte"), "<p>Hello</p>").unwrap();
    let base = json!({"root":root,"dependencies":deps,"components":[{"path":"A.svelte","typescript":false}],"runes":true});
    let mut config: Config = serde_json::from_value(base.clone()).unwrap();
    config.validate().unwrap();
    let settings = config.session_configuration(&json!({})).unwrap();
    assert_eq!(settings["compatibility"]["svelteVersion"], "5.53.11");
    assert_eq!(settings["runes"], true);
    assert!(
        config
            .session_configuration(&json!({"runes":false}))
            .is_err()
    );
    assert!(
        config
            .session_configuration(&json!({"compatibility":{"svelteVersion":"5.57.2"}}))
            .is_err()
    );
    for path in [
        "A.svelte",
        "../escape.svelte",
        "__svelte/types.d.ts",
        "node_modules/x.ts",
        "tsconfig.json",
    ] {
        let mut input = base.clone();
        input["files"] = json!([path]);
        let mut invalid: Config = serde_json::from_value(input).unwrap();
        assert!(invalid.validate().is_err(), "accepted {path}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", root.join("escape.svelte")).unwrap();
        let mut input = base;
        input["files"] = json!(["escape.svelte"]);
        let mut invalid: Config = serde_json::from_value(input).unwrap();
        assert!(invalid.validate().is_err());
    }
    std::fs::write(deps.join("svelte/package.json"), r#"{"version":"5.57.2"}"#).unwrap();
    assert!(config.session_configuration(&json!({})).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

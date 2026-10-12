use super::*;

#[test]
fn resolve_matches_node_path_resolve() {
    let root = Path::new("/app");
    assert_eq!(resolve(root, "./src/lib"), PathBuf::from("/app/src/lib"));
    assert_eq!(resolve(root, "../x/./y"), PathBuf::from("/x/y"));
    assert_eq!(resolve(root, "/abs/z"), PathBuf::from("/abs/z"));
}

fn plugin_project(name: &str) -> (PathBuf, ProjectInput) {
    let root = std::env::temp_dir().join(format!("ts-svelte-build-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("plugins")).unwrap();
    std::fs::write(
        root.join("vite.config.js"),
        "import { sveltekit } from '@sveltejs/kit/vite';\n\
         import { library } from './plugins/library.mjs';\n\
         export default { plugins: [library(), sveltekit()] };\n",
    )
    .unwrap();
    std::fs::write(root.join("library.js"), "export default 1;\n").unwrap();
    let input = ProjectInput {
        root: root.clone(),
        mode: "production".into(),
        environment: BTreeMap::new(),
        version_name: None,
        define: BTreeMap::new(),
        plugin_modules: vec![],
    };
    (root, input)
}

#[test]
fn unknown_plugin_is_a_requirement_until_its_modules_are_supplied() {
    let (root, mut input) = plugin_project("plugin-modules");
    let mut loader = Loader {
        root: root.clone(),
        requirements: vec![],
    };
    let vite = vite_config(&mut loader, &root, &input, false).unwrap();
    assert!(vite.aliases.is_empty());
    assert_eq!(
        loader
            .requirements
            .iter()
            .map(|r| r.message.as_str())
            .collect::<Vec<_>>(),
        ["Vite plugin ./plugins/library.mjs#library has no native equivalent"]
    );

    input.plugin_modules = vec![PluginModules {
        plugin: "./plugins/library.mjs#library".into(),
        modules: BTreeMap::from([("virtual:library".into(), root.join("library.js"))]),
    }];
    let mut loader = Loader {
        root: root.clone(),
        requirements: vec![],
    };
    let vite = vite_config(&mut loader, &root, &input, false).unwrap();
    assert!(loader.requirements.is_empty());
    assert_eq!(
        vite.aliases,
        [("virtual:library".to_owned(), root.join("library.js"))]
    );

    // A supplied plugin the config does not call, or a missing file, is an error.
    input.plugin_modules[0].plugin = "./plugins/other.mjs#other".into();
    let mut loader = Loader {
        root: root.clone(),
        requirements: vec![],
    };
    assert!(vite_config(&mut loader, &root, &input, false).is_err());
    input.plugin_modules[0].plugin = "./plugins/library.mjs#library".into();
    input.plugin_modules[0].modules =
        BTreeMap::from([("virtual:library".into(), root.join("missing.js"))]);
    let mut loader = Loader {
        root: root.clone(),
        requirements: vec![],
    };
    assert!(vite_config(&mut loader, &root, &input, false).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}

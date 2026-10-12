use super::*;
use std::fs;

struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn request(&self) -> HostRequest {
        HostRequest {
            root: self.0.path().into(),
            entry: "styles/app.css".into(),
            plugins: vec![],
            aliases: vec![],
            module_paths: vec![],
            limits: Default::default(),
        }
    }
    fn names(&self, project: &HostProject) -> BTreeSet<String> {
        project
            .selection
            .selected
            .iter()
            .map(|s| {
                Path::new(&s.path)
                    .strip_prefix(self.0.path())
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/")
            })
            .collect()
    }
}

#[test]
fn resolves_css_style_exports_aliases_and_records_dependency_identity() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@import 'fixture/theme'; @import '~local'; @tailwind utilities source(none);",
    );
    f.write("styles/local.css", "@theme { --color-local: blue; }");
    f.write("node_modules/fixture/package.json",r#"{"name":"fixture","exports":{"./theme":{"style":"./theme.css","default":"./wrong.js"}}}"#);
    f.write(
        "node_modules/fixture/theme.css",
        "@theme { --color-remote: red; }",
    );
    f.write(
        "node_modules/fixture/wrong.js",
        "throw new Error('must never run');",
    );
    let mut request = f.request();
    request.aliases.push(Alias {
        find: "~local$".into(),
        replacement: f.0.path().join("styles/local.css").to_str().unwrap().into(),
    });
    let a = load_project(&request).unwrap();
    let b = load_project(&request).unwrap();
    assert_eq!(a.snapshot.stylesheets.len(), 3);
    assert_eq!(a.snapshot, b.snapshot);
    assert!(
        a.watch
            .files
            .iter()
            .any(|p| p.ends_with("fixture/package.json"))
    );
    assert!(
        a.snapshot
            .stylesheets
            .iter()
            .all(|s| s.id.ends_with(".css"))
    );
    assert!(a.selection.selected.is_empty());
    let css = prepare_stylesheet(&a.snapshot, &a.options)
        .unwrap()
        .build(&["text-remote".into(), "text-local".into()])
        .unwrap()
        .css;
    assert!(css.contains("--color-remote: red"));
    assert!(css.contains(".text-local"));
}

#[test]
fn default_discovery_uses_explicit_root_gitignore_and_preserves_hidden_inputs() {
    let f = Fixture::new();
    f.write("styles/app.css", "@tailwind utilities;");
    f.write("src/App.svelte", "<div class='flex'></div>");
    f.write(".hidden/view.html", "<span class='block'></span>");
    f.write("ignored/view.html", "hidden");
    f.write("node_modules/pkg/view.html", "hidden");
    f.write("public/image.png", "binary by extension");
    f.write(".gitignore", "ignored/\n");
    f.write(".env", "SECRET=must_not_read");
    let result = load_project(&f.request()).unwrap();
    let names = f.names(&result);
    assert!(names.contains("src/App.svelte"));
    assert!(names.contains(".hidden/view.html"));
    assert!(!names.iter().any(|p| p.starts_with("ignored/")
        || p.starts_with("node_modules/")
        || p.ends_with(".png")
        || p == ".env"));
    assert!(
        result
            .unread_environment_files
            .iter()
            .any(|p| p.ends_with("/.env"))
    );
    assert!(
        result
            .watch
            .files
            .iter()
            .any(|p| p.ends_with("/.gitignore"))
    );
    assert!(result.watch.missing.iter().any(|p| p.ends_with("/.ignore")));
    assert!(result.watch.directories.iter().any(|p| p.ends_with("/src")));
}

#[test]
fn explicit_patterns_override_file_ignores_external_sources_and_negatives_win() {
    let f = Fixture::new();
    f.write("styles/app.css","@tailwind utilities source(none); @source '../src/*.html'; @source '../node_modules/pkg'; @source not '../src/no.html';");
    f.write(".gitignore", "*.html\n");
    f.write("src/yes.html", "<div class='flex'>");
    f.write("src/no.html", "<div class='grid'>");
    f.write("node_modules/pkg/component.svelte", "<div class='block'>");
    f.write("node_modules/pkg/photo.png", "ignored image");
    let result = load_project(&f.request()).unwrap();
    assert_eq!(
        f.names(&result),
        BTreeSet::from([
            "src/yes.html".into(),
            "node_modules/pkg/component.svelte".into()
        ])
    );
    assert!(result.inventory.unreachable.is_empty());
}

#[test]
fn source_braces_parent_traversal_and_broad_auto_behavior_match_normalization() {
    let f = Fixture::new();
    f.write("styles/app.css","@tailwind utilities source(none); @source '../src/*/deep/**/*'; @source '../other/{a,b}.html';");
    f.write("src/unrelated.html", "block");
    f.write("src/a/deep/view.svelte", "flex");
    f.write("other/a.html", "grid");
    f.write("other/b.html", "hidden");
    f.write("other/c.html", "no");
    let result = load_project(&f.request()).unwrap();
    assert_eq!(
        f.names(&result),
        BTreeSet::from([
            "src/unrelated.html".into(),
            "src/a/deep/view.svelte".into(),
            "other/a.html".into(),
            "other/b.html".into()
        ])
    );
    assert!(
        result
            .selection
            .sources
            .iter()
            .any(|s| s.base.ends_with("/src") && s.pattern == "/**/*")
    );
}

#[test]
fn exact_source_names_do_not_turn_glob_escapes_into_control_characters() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@tailwind utilities source(none); @source '../src/{apple,banana,newline,return,tab,éclair}.html'; @source not '../src/return.html';",
    );
    for name in ["apple", "banana", "newline", "return", "tab", "éclair"] {
        f.write(&format!("src/{name}.html"), "flex");
    }
    let result = load_project(&f.request()).unwrap();
    assert_eq!(
        f.names(&result),
        ["apple", "banana", "newline", "tab", "éclair"]
            .into_iter()
            .map(|name| format!("src/{name}.html"))
            .collect()
    );
}

#[test]
fn unresolved_imports_configs_plugins_and_environment_sources_are_pathful_errors() {
    let f = Fixture::new();
    f.write("styles/app.css", "@import './missing.css';");
    let error = load_project(&f.request()).unwrap_err();
    assert_eq!(error.kind, "resolve");
    assert!(error.path.ends_with("app.css"));
    assert!(
        error
            .watch
            .missing
            .iter()
            .any(|p| p.ends_with("missing.css"))
    );
    for (css, kind) in [
        ("@config '../tailwind.config.js';", "unsupported"),
        ("@plugin 'unknown-plugin';", "stylesheet"),
        (
            "@tailwind utilities source(none); @source '../.env';",
            "protectedInput",
        ),
    ] {
        f.write("styles/app.css", css);
        let error = load_project(&f.request()).unwrap_err();
        assert_eq!(error.kind, kind, "{error}");
        assert!(!error.path.is_empty());
    }
}

#[test]
fn repeated_snapshots_reflect_new_files_repaired_imports_and_content_versions() {
    let f = Fixture::new();
    f.write("styles/app.css", "@tailwind utilities;");
    f.write("src/a.html", "flex");
    let first = load_project(&f.request()).unwrap();
    let old = first
        .snapshot
        .sources
        .iter()
        .find(|s| s.id.ends_with("/a.html"))
        .unwrap()
        .version;
    f.write("src/a.html", "grid");
    f.write("src/b.html", "block");
    let second = load_project(&f.request()).unwrap();
    let new = second
        .snapshot
        .sources
        .iter()
        .find(|s| s.id.ends_with("/a.html"))
        .unwrap()
        .version;
    assert_ne!(old, new);
    assert!(f.names(&second).contains("src/b.html"));
    f.write(
        "styles/app.css",
        "@import './late.css'; @tailwind utilities source(none);",
    );
    assert!(load_project(&f.request()).is_err());
    f.write("styles/late.css", "@theme { --color-late: red; }");
    assert_eq!(
        load_project(&f.request())
            .unwrap()
            .snapshot
            .stylesheets
            .len(),
        2
    );
}

#[test]
fn resource_limits_and_import_cycles_fail_without_partial_success() {
    let f = Fixture::new();
    f.write("styles/app.css", "@tailwind utilities;");
    f.write("src/a.html", "a");
    f.write("src/b.html", "b");
    let mut request = f.request();
    request.limits.files = 1;
    assert_eq!(load_project(&request).unwrap_err().kind, "limit");
    request.limits = HostLimits::default();
    request.limits.file_bytes = 4;
    assert_eq!(load_project(&request).unwrap_err().kind, "limit");
    f.write("styles/app.css", "@import './cycle.css';");
    f.write("styles/cycle.css", "@import './app.css';");
    assert_eq!(load_project(&f.request()).unwrap_err().kind, "stylesheet");
}

#[cfg(unix)]
#[test]
fn symlinked_sources_share_canonical_identity_and_follow_external_targets() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let external = Fixture::new();
    external.write("outside.html", "flex");
    f.write(
        "styles/app.css",
        "@tailwind utilities source(none); @source '../src';",
    );
    fs::create_dir(f.0.path().join("src")).unwrap();
    symlink(
        external.0.path().join("outside.html"),
        f.0.path().join("src/link.html"),
    )
    .unwrap();
    let result = load_project(&f.request()).unwrap();
    assert_eq!(result.selection.selected.len(), 1);
    assert_eq!(
        result.selection.selected[0].id,
        path_string(&canonical(&external.0.path().join("outside.html")).unwrap()).unwrap()
    );
    assert_eq!(result.snapshot.sources[0].text, "flex");
}

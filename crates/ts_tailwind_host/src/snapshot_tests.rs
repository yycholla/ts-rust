use super::*;
use crate::compile::{CompiledProject, compile_project_with_snapshots};
use oxc_sourcemap::SourceMapBuilder;

struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
    fn write(&self, name: &str, text: &str) {
        let path = self.path(name);
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
    fn file(&self, name: &str, text: &str, version: i64) -> FileSnapshot {
        FileSnapshot {
            path: self.path(name),
            text: text.into(),
            version,
            map: None,
        }
    }
    fn compile(&self, files: Vec<FileSnapshot>) -> CompiledProject {
        compile_project_with_snapshots(
            &self.request(),
            &Default::default(),
            &HostSnapshots {
                files,
                ..Default::default()
            },
        )
        .unwrap()
    }
}

#[test]
fn authoritative_css_and_component_edits_keep_signed_versions() {
    let f = Fixture::new();
    f.write("styles/app.css", ".obsolete {color: red}");
    f.write("src/App.svelte", "<div class='grid'></div>");
    let result = f.compile(vec![
        f.file("styles/app.css", "@tailwind utilities;", -17),
        f.file("src/App.svelte", "<div class='flex'></div>", -29),
    ]);
    assert!(result.build.stylesheet.css.contains(".flex"));
    assert!(!result.build.stylesheet.css.contains(".grid"));
    assert!(!result.build.stylesheet.css.contains("obsolete"));
    assert_eq!(result.project.snapshot.stylesheets[0].version, -17);
    assert_eq!(
        result
            .project
            .snapshot
            .sources
            .iter()
            .find(|s| s.id.ends_with("App.svelte"))
            .unwrap()
            .version,
        -29
    );
    assert_eq!(
        result.project.watch.snapshots[&path_string(&f.path("src/App.svelte")).unwrap()].version,
        -29
    );
    assert_eq!(
        fs::read_to_string(f.path("src/App.svelte")).unwrap(),
        "<div class='grid'></div>"
    );
}

#[test]
fn new_entry_import_and_source_directories_exist_only_in_snapshot() {
    let f = Fixture::new();
    let result = f.compile(vec![
        f.file(
            "styles/app.css",
            "@import './nested/theme.css'; @tailwind utilities;",
            3,
        ),
        f.file(
            "styles/nested/theme.css",
            "@theme { --color-sky: #123456; }",
            -4,
        ),
        f.file(
            "new/deep/App.svelte",
            "<main class='text-sky flex'></main>",
            -5,
        ),
    ]);
    assert!(result.build.stylesheet.css.contains(".text-sky"));
    assert!(result.build.stylesheet.css.contains("#123456"));
    assert!(result.build.stylesheet.css.contains(".flex"));
    assert!(!f.path("styles").exists());
    assert!(!f.path("new").exists());
    assert!(
        result
            .project
            .snapshot
            .imports
            .iter()
            .any(|i| i.target.ends_with("nested/theme.css"))
    );
}

#[test]
fn ignored_virtual_ancestor_and_negative_sources_are_not_scanned() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@tailwind utilities; @source not '../src/negative/**';",
    );
    f.write(".gitignore", "ignored/\n");
    let result = f.compile(vec![
        f.file("ignored/nested/App.svelte", "<div class='grid'>", 1),
        f.file("src/negative/App.svelte", "<div class='block'>", 2),
        f.file(".hidden/App.svelte", "<div class='flex'>", 3),
    ]);
    assert!(result.build.stylesheet.css.contains(".flex"));
    assert!(!result.build.stylesheet.css.contains(".grid"));
    assert!(!result.build.stylesheet.css.contains(".block"));
    assert!(
        !result
            .project
            .snapshot
            .sources
            .iter()
            .any(|s| s.id.contains("/ignored/") || s.id.contains("/negative/"))
    );
}

#[test]
fn explicit_external_and_virtual_root_sources_use_scanner_rules() {
    let f = Fixture::new();
    f.write("styles/app.css", "@tailwind utilities source(none); @source '../node_modules/widgets'; @source not '../node_modules/widgets/no/**';");
    let result = f.compile(vec![
        f.file("node_modules/widgets/App.svelte", "<div class='grid'>", 1),
        f.file(
            "node_modules/widgets/no/App.svelte",
            "<div class='flex'>",
            2,
        ),
        f.file("src/App.svelte", "<div class='block'>", 3),
    ]);
    assert!(result.build.stylesheet.css.contains(".grid"));
    assert!(!result.build.stylesheet.css.contains(".flex"));
    assert!(!result.build.stylesheet.css.contains(".block"));
}

#[test]
fn extracted_reference_apply_has_distinct_id_full_component_source_and_map() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@theme { --color-brand: #123456; } @tailwind utilities;",
    );
    let css = "@reference '../styles/app.css';\n.x { @apply flex text-brand; }";
    let prefix = "<div class='grid'>🦀</div>\n<style>\n";
    let original = format!("{prefix}{css}\n</style>");
    let origin = path_string(&f.path("src/App.svelte")).unwrap();
    let id = format!("{origin}?style=0");
    let mut map = SourceMapBuilder::default();
    let source = map.add_source_and_content(&origin, &original);
    for (line, text) in css.split('\n').enumerate() {
        let mut col = 0;
        for c in text.chars() {
            map.add_token(line as u32, col, line as u32 + 2, col, Some(source), None);
            col += c.len_utf16() as u32;
        }
    }
    let input = HostSnapshots {
        files: vec![f.file("src/App.svelte", &original, -8)],
        entry: Some(EntryStylesheet {
            id: id.clone(),
            origin: f.path("src/App.svelte"),
            version: -8,
            css: css.into(),
            map: Some(serde_json::from_str(&map.into_owned_sourcemap().to_json_string()).unwrap()),
        }),
        ..Default::default()
    };
    let result = compile_project_with_snapshots(&f.request(), &Default::default(), &input).unwrap();
    assert!(result.build.stylesheet.css.contains("display: flex"));
    assert!(
        result
            .build
            .stylesheet
            .css
            .contains("var(--color-brand, #123456)")
    );
    assert!(!result.build.stylesheet.css.contains("@apply"));
    assert_eq!(result.project.snapshot.entry, id);
    assert_eq!(
        result
            .project
            .snapshot
            .sources
            .iter()
            .find(|s| s.id == origin)
            .unwrap()
            .text,
        original
    );
    assert!(
        result.map["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == &origin)
    );
    assert!(
        !result.map["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == &id)
    );
}

#[test]
fn virtual_imported_inline_theme_url_uses_original_asset_base() {
    let f = Fixture::new();
    f.write(
        "styles/app.css",
        "@import '../package/theme.css'; @tailwind utilities;",
    );
    f.write("package/images/icon.svg", "<svg/>");
    let result = f.compile(vec![
        f.file(
            "package/theme.css",
            "@theme inline { --background-image-icon: url('./images/icon.svg?v=1#glyph'); }",
            -1,
        ),
        f.file("src/App.svelte", "<div class='bg-icon'>", -2),
    ]);
    assert_eq!(result.assets.len(), 1);
    assert_eq!(
        result.assets[0].path,
        path_string(&f.path("package/images/icon.svg")).unwrap()
    );
    assert_eq!(result.assets[0].query.as_deref(), Some("v=1"));
    assert_eq!(result.assets[0].fragment.as_deref(), Some("glyph"));
    let theme_id = path_string(&f.path("package/theme.css")).unwrap();
    assert!(
        result.map["x_ts_rust_documentOnlySources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == &theme_id)
    );
    let decoded =
        oxc_sourcemap::SourceMap::from_json(serde_json::from_value(result.map.clone()).unwrap())
            .unwrap();
    let theme_index = decoded.get_sources().position(|id| id == theme_id).unwrap() as u32;
    assert!(
        decoded
            .get_source_view_tokens()
            .all(|token| token.get_source_id() != Some(theme_index))
    );

    assert!(
        result.assets[0].occurrences[0]
            .file
            .ends_with("package/theme.css")
    );
    assert!(
        result.map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().is_some_and(|s| s.contains("./images/icon.svg")))
    );
}

#[test]
fn asset_text_overlay_is_never_replaced_by_disk_bytes() {
    let f = Fixture::new();
    f.write("styles/app.css", ".a {background:url('./asset.svg')}");
    f.write("styles/asset.svg", "old");
    let error = compile_project_with_snapshots(
        &f.request(),
        &Default::default(),
        &HostSnapshots {
            files: vec![f.file("styles/asset.svg", "new", 2)],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.message.contains("authoritative text snapshot"));
}

#[test]
fn native_alias_and_vite_export_conditions_resolve_virtual_css() {
    let f = Fixture::new();
    f.write("styles/app.css", "@import 'palette'; @import 'pkg';");
    f.write("node_modules/pkg/package.json", r#"{"exports":{".":{"development":"./dev.css","production":"./prod.css","default":"./default.css"}}}"#);
    let mut request = f.request();
    request.aliases.push(Alias {
        find: "palette$".into(),
        replacement: path_string(&f.path("virtual/palette.css")).unwrap(),
    });
    let files = vec![
        f.file("virtual/palette.css", ".palette {color:red}", 1),
        f.file("node_modules/pkg/dev.css", ".development {color:green}", 2),
        f.file("node_modules/pkg/prod.css", ".production {color:blue}", 3),
        f.file("node_modules/pkg/default.css", ".default {color:black}", 4),
    ];
    let development = compile_project_with_snapshots(
        &request,
        &Default::default(),
        &HostSnapshots {
            files: files.clone(),
            resolver: ResolverProfile::ViteDevelopment,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(development.build.stylesheet.css.contains(".palette"));
    assert!(development.build.stylesheet.css.contains(".development"));
    assert!(!development.build.stylesheet.css.contains(".production"));
    let production = compile_project_with_snapshots(
        &request,
        &Default::default(),
        &HostSnapshots {
            files,
            resolver: ResolverProfile::ViteProduction,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(production.build.stylesheet.css.contains(".production"));
}

#[test]
fn duplicate_parent_file_and_protected_snapshots_fail_explicitly() {
    let f = Fixture::new();
    f.write("styles/app.css", "");
    for files in [
        vec![f.file("new.css", "", 1), f.file("./new.css", "", 2)],
        vec![f.file("new", "", 1), f.file("new/child.css", "", 2)],
        vec![f.file(".env.local", "NEVER_READ=yes", 1)],
        vec![f.file(".gitignore", "src/", 1)],
    ] {
        assert!(
            load_project_with_snapshots(
                &f.request(),
                &HostSnapshots {
                    files,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn existing_symlink_aliases_do_not_create_two_snapshot_identities() {
    let f = Fixture::new();
    f.write("styles/app.css", "");
    fs::create_dir(f.path("real")).unwrap();
    std::os::unix::fs::symlink(f.path("real"), f.path("alias")).unwrap();
    let result = load_project_with_snapshots(
        &f.request(),
        &HostSnapshots {
            files: vec![
                f.file("alias/new.css", "", 1),
                f.file("real/new.css", "", 2),
            ],
            ..Default::default()
        },
    );
    assert!(result.unwrap_err().message.contains("aliased snapshot"));
}

#[test]
fn malformed_or_non_utf16_boundary_input_maps_are_rejected() {
    let f = Fixture::new();
    f.write("styles/app.css", "");
    let mut file = f.file("styles/app.css", ".a{}", 1);
    file.map = Some(
        serde_json::json!({"version":3,"sources":[path_string(&f.path("input.svelte")).unwrap()],"sourcesContent":["🦀"],"names":[],"mappings":"AAAC"}),
    );
    assert!(
        load_project_with_snapshots(
            &f.request(),
            &HostSnapshots {
                files: vec![file],
                ..Default::default()
            }
        )
        .unwrap_err()
        .message
        .contains("UTF-16")
    );
}

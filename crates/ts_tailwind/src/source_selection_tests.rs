use super::*;
use crate::stylesheet::SourceDirective;
fn metadata(root: SourceRoot) -> StylesheetMetadata {
    StylesheetMetadata {
        entry: "style.css".into(),
        entry_base: "/app".into(),
        root,
        sources: Vec::new(),
        inline_candidates: Vec::new(),
        excluded_candidates: Vec::new(),
        imported_stylesheets: Vec::new(),
        imports: Vec::new(),
        has_utilities: true,
        important: false,
    }
}
fn fixture(paths: &[(&str, &str, bool, bool)]) -> (Vec<SourceSnapshot>, SourceInventory) {
    let snapshots = paths
        .iter()
        .map(|(id, _, _, _)| SourceSnapshot {
            id: id.to_string(),
            version: -1,
            extension: "svelte".into(),
            text: "<div class='flex'></div>".into(),
        })
        .collect();
    let inventory = SourceInventory {
        files: paths
            .iter()
            .map(|(id, path, binary, _)| SourcePath {
                id: id.to_string(),
                path: path.to_string(),
                binary: *binary,
            })
            .collect(),
        directories: vec!["/app".into()],
        automatic_eligible_ids: paths
            .iter()
            .filter(|(_, _, _, auto)| *auto)
            .map(|(id, _, _, _)| id.to_string())
            .collect(),
        unreachable: Vec::new(),
    };
    (snapshots, inventory)
}
fn source(meta: &mut StylesheetMetadata, pattern: &str, negated: bool) {
    meta.sources.push(SourceDirective {
        base: "/app".into(),
        pattern: pattern.into(),
        negated,
        source: None,
    });
}
fn ids(result: &SourceSelection) -> Vec<&str> {
    result.selected.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn auto_root_is_scoped_and_honors_host_eligibility_without_hiding_dotfiles() {
    let (snapshots, inventory) = fixture(&[
        ("visible", "/app/src/a.svelte", false, true),
        ("hidden", "/app/.hidden/b.svelte", false, true),
        ("ignored", "/app/ignored.svelte", false, false),
        ("external", "/other/a.svelte", false, true),
        ("binary", "/app/data.bin", true, false),
    ]);
    let result = select_sources(&metadata(SourceRoot::Auto), &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["hidden", "visible"]);
    assert!(
        result
            .selected
            .iter()
            .all(|s| s.reasons[0].mode == SelectionMode::Automatic)
    );
    assert_eq!(result.sources[0].base, "/app");
    assert_eq!(result.sources[0].pattern, "/**/*");
}

#[test]
fn none_requires_explicit_sources_and_literal_directory_is_not_all_inventory() {
    let (snapshots, inventory) = fixture(&[
        ("app", "/app/src/a.svelte", false, true),
        ("other", "/app/other/a.svelte", false, true),
    ]);
    let mut meta = metadata(SourceRoot::None);
    assert!(
        select_sources(&meta, &snapshots, &inventory)
            .unwrap()
            .selected
            .is_empty()
    );
    source(&mut meta, "src", false);
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["app"]);
    assert_eq!(
        result.selected[0].reasons[0].origin,
        SourceOrigin::Directive { index: 0 }
    );
}

#[test]
fn specific_files_and_patterns_override_auto_exclusions_but_external_dirs_skip_binary() {
    let (snapshots, inventory) = fixture(&[
        ("ignored", "/app/generated/file.svelte", false, false),
        ("lib", "/app/node_modules/lib/a.svelte", false, false),
        ("lib-bin", "/app/node_modules/lib/data.bin", true, false),
        ("explicit-bin", "/app/exact.bin", true, false),
    ]);
    let mut meta = metadata(SourceRoot::None);
    source(&mut meta, "generated/*.svelte", false);
    source(&mut meta, "node_modules/lib", false);
    source(&mut meta, "exact.bin", false);
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["explicit-bin", "ignored", "lib"]);
    assert_eq!(result.selected[2].reasons[0].mode, SelectionMode::External);
    assert_eq!(result.selected[0].reasons[0].mode, SelectionMode::File);
}

#[test]
fn negative_directories_and_patterns_win_over_every_positive_source() {
    let (snapshots, inventory) = fixture(&[
        ("a", "/app/src/a.svelte", false, true),
        ("nested", "/app/src/ignore/b.svelte", false, true),
        ("spec", "/app/src/a.spec.svelte", false, true),
    ]);
    let mut meta = metadata(SourceRoot::Auto);
    source(&mut meta, "src/ignore", true);
    source(&mut meta, "src/**/*.spec.svelte", true);
    source(&mut meta, "src/ignore/b.svelte", false);
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["a"]);
    assert_eq!(result.excluded.len(), 2);
    assert!(
        result
            .excluded
            .iter()
            .all(|s| s.reasons[0].mode == SelectionMode::Excluded)
    );
}

#[test]
fn recursive_glob_extension_override_and_host_ancestor_reachability() {
    let (snapshots, mut inventory) = fixture(&[
        ("ignored-file", "/app/generated/a.svelte", false, false),
        ("blocked", "/app/blocked/a.svelte", false, false),
        ("binary", "/app/data.bin", true, false),
        ("auto", "/app/src/a.svelte", false, true),
    ]);
    let mut meta = metadata(SourceRoot::None);
    source(&mut meta, "**/*.svelte", false);
    inventory.unreachable.push(UnreachableSources {
        origin: SourceOrigin::Directive { index: 0 },
        ids: vec!["blocked".into()],
    });
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["ignored-file", "auto"]);
    meta.sources[0].pattern = "**".into();
    assert_eq!(
        ids(&select_sources(&meta, &snapshots, &inventory).unwrap()),
        ["auto"]
    );
    meta.sources[0].pattern = "**/*.bin".into();
    assert_eq!(
        ids(&select_sources(&meta, &snapshots, &inventory).unwrap()),
        ["binary"]
    );
}

#[test]
fn brace_ranges_classes_and_question_marks_match_pinned_glob_grammar() {
    let (snapshots, inventory) = fixture(&[
        ("one", "/app/src/page1.svelte", false, false),
        ("two", "/app/src/page2.svelte", false, false),
        ("three", "/app/src/page3.svelte", false, false),
        ("alpha", "/app/src/a.html", false, false),
        ("beta", "/app/src/b.html", false, false),
        ("nested", "/app/src/nested/a.html", false, false),
    ]);
    let mut meta = metadata(SourceRoot::None);
    source(&mut meta, "src/page{1..2}.svelte", false);
    source(&mut meta, "src/[ab].html", false);
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["alpha", "beta", "one", "two"]);
    meta.sources.clear();
    source(&mut meta, "src/?.html", false);
    assert_eq!(
        ids(&select_sources(&meta, &snapshots, &inventory).unwrap()),
        ["alpha", "beta"]
    );
    assert_eq!(
        expand_braces("{a..c}.html").unwrap(),
        ["a.html", "b.html", "c.html"]
    );
    assert_eq!(
        expand_braces("{=01..03}.html").unwrap(),
        ["01.html", "02.html", "03.html"]
    );
}

#[test]
fn source_root_parent_traversal_and_dynamic_auto_base_are_resolved_explicitly() {
    let (snapshots, inventory) = fixture(&[
        ("shared", "/shared/a.svelte", false, true),
        ("local", "/app/src/other/a.svelte", false, true),
        ("wanted", "/app/src/part/foo/b.svelte", false, true),
    ]);
    let meta = metadata(SourceRoot::Pattern {
        base: "/app".into(),
        pattern: "../shared".into(),
    });
    assert_eq!(
        ids(&select_sources(&meta, &snapshots, &inventory).unwrap()),
        ["shared"]
    );
    let mut meta = metadata(SourceRoot::None);
    source(&mut meta, "src/*/foo/**/*", false);
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    // Pinned Oxide classifies the normalized **/* suffix as Auto at /app/src.
    assert_eq!(ids(&result), ["local", "wanted"]);
    assert_eq!(result.sources[0].base, "/app/src");
    assert_eq!(result.sources[0].pattern, "/**/*");
}

#[test]
fn paths_are_platform_independent_normalized_and_case_sensitive() {
    let (snapshots, mut inventory) = fixture(&[
        ("win", "C:/project/src/A.svelte", false, true),
        ("unc", "//server/share/src/B.svelte", false, true),
    ]);
    inventory.directories.clear();
    let mut meta = metadata(SourceRoot::Pattern {
        base: "C:/project".into(),
        pattern: "src".into(),
    });
    assert_eq!(
        ids(&select_sources(&meta, &snapshots, &inventory).unwrap()),
        ["win"]
    );
    meta.root = SourceRoot::Pattern {
        base: "//server/share".into(),
        pattern: "src/*.svelte".into(),
    };
    assert_eq!(
        ids(&select_sources(&meta, &snapshots, &inventory).unwrap()),
        ["unc"]
    );
    for bad in [
        "relative/a.svelte",
        "c:/project/a.svelte",
        "C:\\project\\a.svelte",
        "/app/../a.svelte",
        "/app//a.svelte",
        "//server/../a.svelte",
    ] {
        assert!(canonical(bad).is_err(), "{bad}");
    }
}

#[test]
fn literal_base_metacharacters_remain_literal_across_parent_traversal() {
    let (snapshots, mut inventory) = fixture(&[
        ("wanted", "/app/[root]/src/a.svelte", false, true),
        ("other", "/app/r/src/a.svelte", false, true),
    ]);
    inventory.directories.push("/app/[root]/nested".into());
    let meta = metadata(SourceRoot::Pattern {
        base: "/app/[root]/nested".into(),
        pattern: "../src/*.svelte".into(),
    });
    let result = select_sources(&meta, &snapshots, &inventory).unwrap();
    assert_eq!(ids(&result), ["wanted"]);
    assert_eq!(result.sources[0].base, "/app/[root]/src");
    assert_eq!(result.sources[0].pattern, "/*.svelte");
}

#[test]
fn missing_duplicate_ambiguous_or_unresolved_inputs_fail_instead_of_broadening() {
    let (snapshots, inventory) = fixture(&[("a", "/app/a.svelte", false, true)]);
    let mut missing = inventory.clone();
    missing.files.clear();
    assert_eq!(
        select_sources(&metadata(SourceRoot::Auto), &snapshots, &missing)
            .unwrap_err()
            .kind,
        SelectionErrorKind::Inventory
    );
    let mut duplicate = inventory.clone();
    duplicate.files.push(duplicate.files[0].clone());
    assert!(select_sources(&metadata(SourceRoot::Auto), &snapshots, &duplicate).is_err());
    let mut ambiguous = inventory.clone();
    ambiguous.directories.push("/app/a.svelte".into());
    assert!(select_sources(&metadata(SourceRoot::Auto), &snapshots, &ambiguous).is_err());
    let mut meta = metadata(SourceRoot::None);
    source(&mut meta, "missing", false);
    assert_eq!(
        select_sources(&meta, &snapshots, &inventory)
            .unwrap_err()
            .kind,
        SelectionErrorKind::MissingPath
    );
    for unsupported in [
        "@(a|b).svelte",
        "**.svelte",
        "[bad",
        "*/../a.svelte",
        "../../../../a.svelte",
    ] {
        meta.sources[0].pattern = unsupported.into();
        assert!(
            select_sources(&meta, &snapshots, &inventory).is_err(),
            "{unsupported}"
        );
    }
}

#[test]
fn explicit_empty_directories_zero_matches_and_inventory_order_are_stable() {
    let (mut snapshots, mut inventory) = fixture(&[
        ("b", "/app/src/b.svelte", false, true),
        ("a", "/app/src/a.svelte", false, true),
    ]);
    inventory.directories.push("/app/empty".into());
    let mut meta = metadata(SourceRoot::None);
    source(&mut meta, "empty", false);
    source(&mut meta, "src/*.html", false);
    assert!(
        select_sources(&meta, &snapshots, &inventory)
            .unwrap()
            .selected
            .is_empty()
    );
    source(&mut meta, "src", false);
    let expected = select_sources(&meta, &snapshots, &inventory).unwrap();
    snapshots.reverse();
    inventory.files.reverse();
    inventory.automatic_eligible_ids.reverse();
    assert_eq!(
        expected,
        select_sources(&meta, &snapshots, &inventory).unwrap()
    );
    assert_eq!(ids(&expected), ["a", "b"]);
}

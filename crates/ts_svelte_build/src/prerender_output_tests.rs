use super::*;
use ts_svelte_kit::prerender_save::{
    Category, HttpErrorPolicy, ReferenceType, SaveConfig, SaveEffect, SaveInput, SaveState,
    SaveStatus,
};

struct Fixture {
    save: SaveSnapshot,
    queue: QueueSnapshot,
    files: BTreeMap<String, Vec<u8>>,
    completion: PolicyResult,
    routes: Vec<String>,
}
impl Fixture {
    fn input(&self) -> CompletedPrerender<'_> {
        CompletedPrerender {
            completion: &self.completion,
            save: &self.save,
            queue: &self.queue,
            files: &self.files,
            base: "/base",
            route_ids: &self.routes,
        }
    }
}
fn apply(
    state: &mut SaveState,
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    category: Category,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let result = state
        .save(SaveInput {
            decoded: format!("/base{path}"),
            encoded: format!("/base{path}"),
            referrer: None,
            reference_type: ReferenceType::Linked,
            category,
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_vec(),
        })
        .unwrap();
    assert!(
        matches!(
            result.status,
            SaveStatus::Complete | SaveStatus::Duplicate { .. }
        ),
        "{result:?}"
    );
    for effect in result.effects {
        if let SaveEffect::Write { path, body } = effect {
            files.insert(path, body);
        }
    }
}
fn fixture() -> Fixture {
    let mut state = SaveState::new(
        SaveConfig {
            kit_version: "2.69.2".into(),
            base: "/base".into(),
            origin: "https://example.test".into(),
            http_error: HttpErrorPolicy::Ignore,
        },
        BTreeMap::new(),
    )
    .unwrap();
    let mut files = BTreeMap::new();
    apply(
        &mut state,
        &mut files,
        "/",
        Category::Pages,
        200,
        &[("content-type", "text/html"), ("x-sveltekit-routeid", "/")],
        b"<h1>real</h1>",
    );
    apply(
        &mut state,
        &mut files,
        "/file.bin",
        Category::Dependencies,
        200,
        &[("content-type", "application/octet-stream")],
        &[0, 255, 128],
    );
    apply(
        &mut state,
        &mut files,
        "/_app/remote/f/arg",
        Category::Data,
        200,
        &[("content-type", "application/json")],
        b"[1]",
    );
    apply(
        &mut state,
        &mut files,
        "/old",
        Category::Pages,
        308,
        &[("location", "/base/")],
        b"ignored redirect body",
    );
    apply(
        &mut state,
        &mut files,
        "/nothing",
        Category::Dependencies,
        204,
        &[],
        b"ignored 204 body",
    );
    apply(
        &mut state,
        &mut files,
        "/ignored",
        Category::Dependencies,
        404,
        &[],
        b"ignored error body",
    );
    apply(
        &mut state,
        &mut files,
        "/normalized",
        Category::Pages,
        307,
        &[("location", "/base/"), ("x-sveltekit-normalize", "1")],
        b"",
    );
    let save = state.snapshot().clone();
    Fixture {
        queue: QueueSnapshot {
            prerender_map: vec![
                ("/".into(), PrerenderValue::True),
                ("/auto".into(), PrerenderValue::Auto),
                ("/live".into(), PrerenderValue::False),
            ],
            prerendered_routes: save.prerendered_routes.clone(),
            ..QueueSnapshot::default()
        },
        save,
        files,
        // This pure fixture supplies a trusted completion receipt; no runtime
        // execution or whole-build completion is asserted by these tests.
        completion: PolicyResult {
            status: Status::Finished,
            effects: vec![],
        },
        routes: vec!["/live".into(), "/auto".into(), "/".into(), "/absent".into()],
    }
}

#[test]
fn materializes_real_binary_writes_and_redirects_without_inventing_alias_files() {
    let fixture = fixture();
    let output = materialize(fixture.input(), &BTreeMap::new()).unwrap();
    assert_eq!(output.artifacts.len(), 4);
    assert_eq!(
        artifacts::bytes(&output.artifacts["prerendered/dependencies/file.bin"]).unwrap(),
        vec![0, 255, 128]
    );
    assert_eq!(output.adapter.dependencies["file.bin"], vec![0, 255, 128]);
    assert_eq!(output.adapter.data["_app/remote/f/arg"], b"[1]");
    assert!(output.adapter.pages.contains_key("old.html"));
    assert!(!output.save.saved.iter().any(|(name, _)| name == "old.html"));
    assert_eq!(
        output.save.redirects,
        vec![("/base/old".into(), (308, "/base/".into()))]
    );
    assert_eq!(
        output.unwritten_aliases,
        vec![
            ("nothing".into(), "dependencies/nothing".into()),
            ("ignored".into(), "dependencies/ignored".into())
        ]
    );
    assert!(
        !output
            .artifacts
            .contains_key("prerendered/dependencies/nothing")
    );
    assert!(
        !output
            .artifacts
            .contains_key("prerendered/pages/normalized.html")
    );
    assert_eq!(
        output.adapter.paths,
        vec![
            "/base/",
            "/base/file.bin",
            "/base/_app/remote/f/arg",
            "/base/old"
        ]
    );
    assert_eq!(output.counts.files, 4);
    assert_eq!(output.counts.page_files, 2);
    assert_eq!(output.counts.dependency_files, 1);
    assert_eq!(output.counts.data_files, 1);
    assert_eq!(output.counts.page_entries, 1);
    assert_eq!(output.counts.asset_entries, 2);
    assert_eq!(output.counts.redirect_entries, 1);
    assert_eq!(
        output.counts.bytes,
        fixture.files.values().map(Vec::len).sum::<usize>()
    );
}

#[test]
fn manifest_route_filter_preserves_inventory_order_auto_false_and_absence() {
    let mut fixture = fixture();
    // A completed custom/warn unseen-route policy can retain true without a
    // written route. Upstream still filters strictly by prerender_map value.
    fixture
        .queue
        .prerender_map
        .push(("/unseen".into(), PrerenderValue::True));
    fixture.routes.push("/unseen".into());
    let output = materialize(fixture.input(), &BTreeMap::new()).unwrap();
    assert_eq!(
        output
            .routes
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["/live", "/auto", "/", "/absent", "/unseen"]
    );
    assert_eq!(
        output
            .routes
            .iter()
            .map(|r| r.include_in_server_manifest)
            .collect::<Vec<_>>(),
        vec![true, true, false, true, false]
    );
    assert_eq!(output.routes[1].prerender, Some(PrerenderValue::Auto));
    assert_eq!(output.routes[3].prerender, None);
}

#[test]
fn output_validation_rejects_unfinished_missing_extra_or_ambiguous_records() {
    for case in [
        "unfinished",
        "missing",
        "extra",
        "category",
        "two-categories",
        "metadata",
        "alias",
        "queue",
        "duplicate",
    ] {
        let mut fixture = fixture();
        match case {
            "unfinished" => fixture.completion.status = Status::Ready,
            "missing" => {
                fixture.files.remove("pages/old.html");
            }
            "extra" => {
                fixture.files.insert("pages/extra.html".into(), vec![]);
            }
            "category" => {
                let b = fixture.files.remove("pages/old.html").unwrap();
                fixture.files.insert("unknown/old.html".into(), b);
            }
            "two-categories" => {
                fixture.files.insert("data/old.html".into(), vec![]);
            }
            "metadata" => fixture.save.pages[0].1 = "wrong.html".into(),
            "alias" => fixture.save.saved[0].1 = "pages/wrong.html".into(),
            "queue" => fixture.queue.prerendered_routes.clear(),
            "duplicate" => fixture.save.written.push("index.html".into()),
            _ => unreachable!(),
        }
        assert!(
            materialize(fixture.input(), &BTreeMap::new()).is_err(),
            "{case}"
        );
    }
}

#[test]
fn conflicting_or_parent_artifacts_fail_atomically_and_identical_bytes_are_valid() {
    let fixture = fixture();
    for collision in [
        "prerendered/pages/old.html",
        "prerendered/pages",
        "prerendered/pages/old.html/child",
    ] {
        let mut existing = BTreeMap::new();
        artifacts::insert(
            &mut existing,
            "server/index.js".into(),
            b"server",
            "javascript",
        )
        .unwrap();
        artifacts::insert(&mut existing, collision.into(), b"conflict", "existing").unwrap();
        let before = serde_json::to_value(&existing).unwrap();
        assert!(
            materialize(fixture.input(), &existing).is_err(),
            "{collision}"
        );
        assert_eq!(serde_json::to_value(&existing).unwrap(), before);
    }
    let mut existing = BTreeMap::new();
    artifacts::insert(
        &mut existing,
        "prerendered/pages/old.html".into(),
        &fixture.files["pages/old.html"],
        "prerender",
    )
    .unwrap();
    assert_eq!(
        materialize(fixture.input(), &existing)
            .unwrap()
            .artifacts
            .len(),
        4
    );
}

#[test]
fn ordered_paths_retain_real_duplicates_from_distinct_html_and_asset_writes() {
    let mut state = SaveState::new(
        SaveConfig {
            kit_version: "3.0.1".into(),
            base: "/base".into(),
            origin: "https://example.test".into(),
            http_error: HttpErrorPolicy::Fail,
        },
        BTreeMap::new(),
    )
    .unwrap();
    let mut files = BTreeMap::new();
    apply(
        &mut state,
        &mut files,
        "/same",
        Category::Dependencies,
        200,
        &[],
        b"asset",
    );
    apply(
        &mut state,
        &mut files,
        "/same",
        Category::Pages,
        200,
        &[("content-type", "text/html; charset=utf-8")],
        b"html",
    );
    let mut fixture = fixture();
    fixture.save = state.snapshot().clone();
    fixture.files = files;
    fixture.queue.prerendered_routes.clear();
    let output = materialize(fixture.input(), &BTreeMap::new()).unwrap();
    assert_eq!(output.adapter.paths, vec!["/base/same", "/base/same"]);
    assert_eq!(output.counts.paths, 2);
    assert_eq!(output.adapter.pages["same.html"], b"html");
    assert_eq!(output.adapter.dependencies["same"], b"asset");
}

#[test]
fn unsafe_output_paths_and_stale_metadata_are_explicit_errors() {
    for path in [
        "pages/../outside",
        "/pages/file",
        "pages/a//b",
        "pages/a\\b",
        "pages/a\0b",
        "pages/a?b",
    ] {
        let mut fixture = fixture();
        fixture.files.insert(path.into(), vec![]);
        assert!(
            materialize(fixture.input(), &BTreeMap::new()).is_err(),
            "{path:?}"
        );
    }
    let mut fixture = fixture();
    fixture.save.paths.reverse();
    assert_eq!(
        materialize(fixture.input(), &BTreeMap::new())
            .unwrap()
            .adapter
            .paths,
        fixture.save.paths
    );
    fixture.save.paths.push("/base/extra".into());
    assert!(
        materialize(fixture.input(), &BTreeMap::new())
            .unwrap_err()
            .contains("paths disagree")
    );
}

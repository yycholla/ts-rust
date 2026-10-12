use super::*;
fn config(version: &str) -> QueueConfig {
    QueueConfig {
        kit_version: version.into(),
        base: "".into(),
        origin: "http://sveltekit-prerender".into(),
        app_dir: "_app".into(),
        router: Router::Pathname,
        concurrency: 1,
        entries: vec!["*".into()],
        handle_missing_id: HandlerPolicy::Fail,
        handle_entry_generator_mismatch: HandlerPolicy::Fail,
        handle_unseen_routes: HandlerPolicy::Fail,
    }
}
fn route(id: &str, prerender: Option<PrerenderValue>, entries: Option<&[&str]>) -> RouteMetadata {
    RouteMetadata {
        id: id.into(),
        prerender,
        entries: entries.map(|entries| entries.iter().map(|s| (*s).into()).collect()),
    }
}
fn enqueue(path: &str) -> EnqueueInput {
    EnqueueInput {
        decoded: path.into(),
        encoded: None,
        referrer: None,
        generated_from_id: None,
    }
}
fn ready(version: &str) -> QueueState {
    let mut config = config(version);
    config.entries.clear();
    let mut state = QueueState::new(config, vec![], AssetSnapshot::default()).unwrap();
    assert_eq!(state.start(true).unwrap().status, StartStatus::Ready);
    state
}

#[test]
fn configured_wildcards_groups_optional_and_runtime_entries_preserve_order() {
    let mut config = config("3.0.1");
    config.entries = vec!["/first".into(), "*".into(), "/tail".into(), "*".into()];
    config.concurrency = 16;
    let routes = vec![
        route(
            "/(group)/[[lang]]/about",
            Some(PrerenderValue::True),
            Some(&["/about", "/fr/about"]),
        ),
        route(
            "/users/[id]",
            Some(PrerenderValue::True),
            Some(&["/users/1", "/users/2"]),
        ),
        route("/auto", Some(PrerenderValue::Auto), None),
        route(
            "/never",
            Some(PrerenderValue::False),
            Some(&["/generated-despite-false"]),
        ),
        route("/none", None, None),
        route("/", Some(PrerenderValue::True), None),
    ];
    let mut state = QueueState::new(config, routes, AssetSnapshot::default()).unwrap();
    let seeded = state.start(false).unwrap();
    assert_eq!(seeded.seeds.len(), 13);
    let requests = state.take_ready().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.decoded.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/first",
            "/about",
            "/auto",
            "/",
            "/tail",
            "/fr/about",
            "/users/1",
            "/users/2",
            "/generated-despite-false"
        ]
    );
    assert_eq!(requests[1].generated_from_id, None); // wildcard won dedup
    assert_eq!(
        requests[5].generated_from_id.as_deref(),
        Some("/(group)/[[lang]]/about")
    );
}

#[test]
fn skip_and_hash_fallback_do_not_run_seeds_or_coverage() {
    let routes = vec![route("/a", Some(PrerenderValue::False), Some(&["/b"]))];
    let mut state = QueueState::new(config("2.69.2"), routes, AssetSnapshot::default()).unwrap();
    assert_eq!(state.start(false).unwrap().status, StartStatus::Skipped);
    assert!(state.snapshot().seen.is_empty());
    assert_eq!(state.finish().unwrap().status, PolicyStatus::Finished);
    let mut config = config("3.0.1");
    config.router = Router::Hash;
    let mut state = QueueState::new(
        config,
        vec![route("/required", Some(PrerenderValue::True), None)],
        AssetSnapshot::default(),
    )
    .unwrap();
    assert_eq!(
        state.start(false).unwrap().status,
        StartStatus::FallbackRequired
    );
    assert!(state.take_ready().is_err());
    assert_eq!(state.finish().unwrap().status, PolicyStatus::Finished);
}

#[test]
fn known_assets_are_seen_before_exclusion_and_include_env_and_server_immutable() {
    let mut config = config("3.0.1");
    config.base = "/😀".into();
    config.entries.clear();
    let assets = AssetSnapshot {
        client_files: vec!["logo.svg".into()],
        server_immutable_files: vec!["chunks/x.js".into()],
    };
    let mut state = QueueState::new(config, vec![], assets).unwrap();
    state.start(true).unwrap();
    for file in ["logo.svg", "_app/env.js", "_app/immutable/chunks/x.js"] {
        let path = format!("/😀/{file}");
        assert_eq!(
            state.enqueue(enqueue(&path)).unwrap(),
            EnqueueResult::Asset { file: file.into() }
        );
        assert_eq!(
            state.enqueue(enqueue(&path)).unwrap(),
            EnqueueResult::Duplicate
        );
    }
    assert_eq!(state.snapshot().seen.len(), 3);
    assert!(state.take_ready().unwrap().is_empty());
}

#[test]
fn encode_uri_preserves_reserved_chars_and_explicit_empty_encoded_falls_back() {
    let mut state = ready("3.0.1");
    let path = "/😀/a b%2F?x=1#id";
    let mut entry = enqueue(path);
    entry.encoded = Some("".into());
    state.enqueue(entry).unwrap();
    let request = state.take_ready().unwrap().pop().unwrap();
    assert_eq!(request.encoded, "/%F0%9F%98%80/a%20b%252F?x=1#id");
    assert_eq!(
        request.url,
        "http://sveltekit-prerender/%F0%9F%98%80/a%20b%252F?x=1#id"
    );
    let mut supplied = enqueue("/decoded");
    supplied.encoded = Some("/supplied%2f".into());
    state.enqueue(supplied).unwrap();
    state.complete_visit(request.token).unwrap();
    assert_eq!(state.take_ready().unwrap()[0].encoded, "/supplied%2f");
}

#[test]
fn fifo_concurrency_and_early_close_match_upstream_queue() {
    let mut config = config("3.0.1");
    config.entries.clear();
    config.concurrency = 2;
    let mut state = QueueState::new(config, vec![], AssetSnapshot::default()).unwrap();
    state.start(true).unwrap();
    for path in ["/a", "/b", "/c"] {
        state.enqueue(enqueue(path)).unwrap();
    }
    let requests = state.take_ready().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(state.take_ready().unwrap().is_empty());
    assert!(state.finish().is_err());
    state.complete_visit(requests[1].token).unwrap();
    let third = state.take_ready().unwrap();
    assert_eq!(third[0].decoded, "/c");
    state.complete_visit(requests[0].token).unwrap();
    assert!(!state.is_closed());
    state.complete_visit(third[0].token).unwrap();
    assert!(state.is_closed());
    assert_eq!(
        state.enqueue(enqueue("/a")).unwrap(),
        EnqueueResult::Duplicate
    );
    assert_eq!(
        state.enqueue(enqueue("/_app/env.js")).unwrap(),
        EnqueueResult::Asset {
            file: "_app/env.js".into()
        }
    );
    assert!(
        state
            .enqueue(enqueue("/late"))
            .unwrap_err()
            .contains("ended")
    );
    assert!(state.snapshot().seen.contains(&"/late".into()));
    assert!(state.complete_visit(third[0].token).is_err());
}

#[test]
fn remote_seeds_use_runtime_payload_order_and_no_arg_still_has_one_entry() {
    let mut state = ready("2.69.2");
    state
        .seed_remote(RemoteSeed {
            id: "hash/name".into(),
            has_arg: true,
            serialized_arguments: vec!["AA".into(), "BB".into(), "AA".into()],
        })
        .unwrap();
    state
        .seed_remote(RemoteSeed {
            id: "hash/none".into(),
            has_arg: false,
            serialized_arguments: vec!["ignored-after-runtime-evaluation".into()],
        })
        .unwrap();
    assert_eq!(
        state.snapshot().seen,
        vec![
            "/_app/remote/hash/name/AA",
            "/_app/remote/hash/name/BB",
            "/_app/remote/hash/none"
        ]
    );
}

#[test]
fn route_header_updates_are_ordered_and_auto_is_absorbing() {
    let mut state = ready("3.0.1");
    state
        .record_dependency_headers(Some("true"), Some("/%C3%A9"))
        .unwrap();
    state
        .record_dependency_headers(Some("false"), Some("/é"))
        .unwrap();
    state
        .record_dependency_headers(Some("true"), Some("/é"))
        .unwrap();
    state
        .record_dependency_headers(Some("true"), Some("/second"))
        .unwrap();
    state
        .record_dependency_headers(Some(""), Some("%malformed"))
        .unwrap();
    state
        .record_dependency_headers(None, Some("%malformed"))
        .unwrap();
    state.record_dependency_headers(Some("true"), None).unwrap();
    assert_eq!(
        state.snapshot().prerender_map,
        vec![
            ("/é".into(), PrerenderValue::Auto),
            ("/second".into(), PrerenderValue::True)
        ]
    );
    assert!(
        state
            .record_dependency_headers(Some("true"), Some("%malformed"))
            .is_err()
    );
    state.record_route("/second").unwrap();
    assert_eq!(state.finish().unwrap().status, PolicyStatus::Finished);
}

#[test]
fn entry_mismatch_handles_missing_empty_encoded_and_static_route_profiles() {
    for version in ["2.69.2", "3.0.1"] {
        let mut state = ready(version);
        state.config.handle_entry_generator_mismatch = HandlerPolicy::Warn;
        let mut input = enqueue("/actual");
        input.generated_from_id = Some("/[slug]".into());
        state.enqueue(input).unwrap();
        let request = state.take_ready().unwrap().pop().unwrap();
        assert_eq!(
            state.check_entry_match(request.token, None).unwrap().status,
            PolicyStatus::Proceed
        );
        let result = state
            .check_entry_match(request.token, Some("/actual"))
            .unwrap();
        let QueueEffect::Warning {
            diagnostic:
                QueueDiagnostic::EntryGeneratorMismatch {
                    message,
                    matched_id,
                    ..
                },
        } = &result.effects[0]
        else {
            panic!()
        };
        assert_eq!(matched_id, "/actual");
        assert_eq!(
            message.contains("`a static route`"),
            version.starts_with('3')
        );
        assert_eq!(
            state
                .check_entry_match(request.token, Some("/%5Bslug%5D"))
                .unwrap()
                .effects
                .len(),
            0
        );
        assert_eq!(
            state
                .check_entry_match(request.token, Some(""))
                .unwrap()
                .effects
                .len(),
            1
        );
        assert!(
            state
                .check_entry_match(request.token, Some("%ED%A0%80"))
                .is_err()
        );
    }
}

#[test]
fn wildcard_duplicate_does_not_acquire_later_generated_route_identity() {
    let routes = vec![
        route("/actual", Some(PrerenderValue::True), None),
        route("/[slug]", Some(PrerenderValue::True), Some(&["/actual"])),
    ];
    let mut state = QueueState::new(config("3.0.1"), routes, AssetSnapshot::default()).unwrap();
    state.start(false).unwrap();
    let request = state.take_ready().unwrap().pop().unwrap();
    assert_eq!(request.generated_from_id, None);
    assert!(
        state
            .check_entry_match(request.token, Some("/other"))
            .unwrap()
            .effects
            .is_empty()
    );
}

#[test]
fn hashlink_checks_only_crawled_pages_with_exact_special_fragments_and_referrer_order() {
    let mut state = ready("3.0.1");
    state.config.handle_missing_id = HandlerPolicy::Warn;
    state
        .record_crawled_ids("/target", vec!["exists".into(), "exists".into()])
        .unwrap();
    assert_eq!(state.snapshot().actual_hashlinks[0].1, vec!["exists"]);
    for key in [
        "/target#exists",
        "/target#",
        "/target#top",
        "/not-crawled#missing",
        "/target#TOP",
    ] {
        state.expect_hashlink(key, "/b").unwrap();
    }
    state.expect_hashlink("/target#TOP", "/a").unwrap();
    state.expect_hashlink("/target#TOP", "/b").unwrap();
    let result = state.finish().unwrap();
    assert_eq!(result.status, PolicyStatus::Finished);
    assert_eq!(result.effects.len(), 1);
    let QueueEffect::Warning {
        diagnostic:
            QueueDiagnostic::MissingId {
                id,
                referrers,
                message,
                ..
            },
    } = &result.effects[0]
    else {
        panic!()
    };
    assert_eq!(id, "TOP");
    assert_eq!(referrers, &vec!["/b".to_string(), "/a".to_string()]);
    assert!(message.contains("`id=\"TOP\"`"));
}

#[test]
fn kit2_crawled_id_arrays_keep_duplicates_and_last_page_record_replaces() {
    let mut state = ready("2.69.2");
    state
        .record_crawled_ids("/a", vec!["id".into(), "id".into()])
        .unwrap();
    assert_eq!(state.snapshot().actual_hashlinks[0].1.len(), 2);
    state
        .record_crawled_ids("/a", vec!["replacement".into()])
        .unwrap();
    assert_eq!(
        state.snapshot().actual_hashlinks,
        vec![("/a".into(), vec!["replacement".into()])]
    );
}

#[test]
fn final_handler_continuations_preserve_missing_before_unseen_and_fail_identity() {
    let mut state = ready("3.0.1");
    state.config.handle_missing_id = HandlerPolicy::Custom {
        name: "missing".into(),
    };
    state.config.handle_unseen_routes = HandlerPolicy::Custom {
        name: "unseen".into(),
    };
    state
        .record_dependency_headers(Some("true"), Some("/required"))
        .unwrap();
    state.record_crawled_ids("/a", vec![]).unwrap();
    state.expect_hashlink("/a#x", "/ref").unwrap();
    let first = state.finish().unwrap();
    let PolicyStatus::AwaitingHandler { token } = first.status else {
        panic!()
    };
    assert!(matches!(
        &first.effects[0],
        QueueEffect::Handler {
            diagnostic: QueueDiagnostic::MissingId { .. },
            ..
        }
    ));
    assert!(state.finish().is_err());
    assert!(state.resume_handler(token + 1, Ok(())).is_err());
    let second = state.resume_handler(token, Ok(())).unwrap();
    assert!(
        matches!(&second.effects[0], QueueEffect::Handler { diagnostic: QueueDiagnostic::UnseenRoutes { routes, .. }, .. } if routes == &["/required"])
    );
    let PolicyStatus::AwaitingHandler { token } = second.status else {
        panic!()
    };
    assert_eq!(
        state
            .resume_handler(token, Err("actual callback failure".into()))
            .unwrap()
            .status,
        PolicyStatus::Failed {
            message: "actual callback failure".into()
        }
    );
    assert!(state.finish().is_err());
}

#[test]
fn mismatch_callback_resumes_visit_without_finishing_traversal() {
    let mut state = ready("3.0.1");
    state.config.handle_entry_generator_mismatch = HandlerPolicy::Custom {
        name: "mismatch".into(),
    };
    let mut entry = enqueue("/a");
    entry.generated_from_id = Some("/[id]".into());
    state.enqueue(entry).unwrap();
    let request = state.take_ready().unwrap().pop().unwrap();
    let PolicyStatus::AwaitingHandler { token } = state
        .check_entry_match(request.token, Some("/a"))
        .unwrap()
        .status
    else {
        panic!()
    };
    assert!(state.complete_visit(request.token).is_err());
    assert_eq!(
        state.resume_handler(token, Ok(())).unwrap().status,
        PolicyStatus::Proceed
    );
    state.record_route("/a").unwrap();
    state.complete_visit(request.token).unwrap();
    assert_eq!(state.finish().unwrap().status, PolicyStatus::Finished);
}

#[test]
fn unseen_requires_exact_true_and_save_observations_not_just_visit_completion() {
    let mut config = config("2.69.2");
    config.entries.clear();
    config.handle_unseen_routes = HandlerPolicy::Warn;
    let routes = vec![
        route("/seen", Some(PrerenderValue::True), None),
        route("/missing", Some(PrerenderValue::True), None),
        route("/auto", Some(PrerenderValue::Auto), None),
        route("/false", Some(PrerenderValue::False), None),
    ];
    let mut state = QueueState::new(config, routes, AssetSnapshot::default()).unwrap();
    state.start(false).unwrap();
    state.record_route("/seen").unwrap();
    let result = state.finish().unwrap();
    assert!(
        matches!(&result.effects[..], [QueueEffect::Warning { diagnostic: QueueDiagnostic::UnseenRoutes { routes, .. } }] if routes == &["/missing"])
    );
}

#[test]
fn outside_base_is_flagged_with_upstream_prefix_semantics() {
    let mut config = config("3.0.1");
    config.base = "/base".into();
    config.entries.clear();
    config.concurrency = 2;
    let mut state = QueueState::new(config, vec![], AssetSnapshot::default()).unwrap();
    state.start(true).unwrap();
    state.enqueue(enqueue("/outside")).unwrap();
    state.enqueue(enqueue("/baseish")).unwrap();
    let requests = state.take_ready().unwrap();
    assert!(!requests[0].within_base);
    assert!(requests[1].within_base);
}

#[test]
fn explicit_limits_versions_and_native_failure_do_not_become_empty_success() {
    let mut bad = config("3.2.0");
    assert!(QueueState::new(bad.clone(), vec![], AssetSnapshot::default()).is_err());
    bad.kit_version = "3.0.1".into();
    bad.concurrency = 0;
    assert!(QueueState::new(bad, vec![], AssetSnapshot::default()).is_err());
    let mut state = ready("3.0.1");
    state.enqueue(enqueue("/a")).unwrap();
    let request = state.take_ready().unwrap().pop().unwrap();
    assert_eq!(
        state
            .fail_visit(request.token, "actual visit failure".into())
            .unwrap()
            .status,
        PolicyStatus::Failed {
            message: "actual visit failure".into()
        }
    );
    assert!(state.finish().is_err());
    assert!(state.enqueue(enqueue("/b")).is_err());
    let mut state = ready("3.0.1");
    state.attempts = MAX_ITEMS;
    assert!(
        state
            .enqueue(enqueue("/limit"))
            .unwrap_err()
            .contains("limit")
    );
}

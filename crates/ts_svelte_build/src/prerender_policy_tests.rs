use super::*;
use ts_svelte_kit::prerender_queue::{PrerenderValue, Router};
use ts_svelte_kit::prerender_save::HttpErrorPolicy;

fn config(version: &str) -> PolicyConfig {
    PolicyConfig {
        save: SaveConfig {
            kit_version: version.into(),
            base: "".into(),
            origin: "http://sveltekit-prerender".into(),
            http_error: HttpErrorPolicy::Fail,
        },
        queue: QueueConfig {
            kit_version: version.into(),
            base: "".into(),
            origin: "http://sveltekit-prerender".into(),
            app_dir: "_app".into(),
            router: Router::Pathname,
            concurrency: 1,
            entries: vec!["/".into()],
            handle_missing_id: HandlerPolicy::Fail,
            handle_entry_generator_mismatch: HandlerPolicy::Fail,
            handle_unseen_routes: HandlerPolicy::Fail,
        },
        crawl: true,
        invalid_url: HandlerPolicy::Fail,
    }
}
fn make_policy(config: PolicyConfig) -> PrerenderPolicy {
    PrerenderPolicy::new(
        config,
        vec![RouteMetadata {
            id: "/".into(),
            prerender: Some(PrerenderValue::Auto),
            entries: None,
        }],
        AssetSnapshot::default(),
        BTreeMap::new(),
    )
    .unwrap()
}
fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Response {
    Response {
        status,
        status_text: vec![],
        headers: headers
            .iter()
            .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect(),
        body: body.to_vec(),
        route_id: None,
    }
}
fn head(response: &Response) -> ResponseHead {
    ResponseHead {
        status: response.status,
        status_text: response.status_text.clone(),
        headers: response.headers.clone(),
        route_id: response.route_id.clone(),
    }
}
fn token(result: &PolicyResult) -> u64 {
    result
        .effects
        .iter()
        .find_map(|e| {
            if let Effect::StartVisit { request } = e {
                Some(request.token)
            } else {
                None
            }
        })
        .unwrap()
}
fn started(policy: &mut PrerenderPolicy) -> u64 {
    assert_eq!(
        policy.start(false),
        PolicyResult {
            status: Status::Initializing,
            effects: vec![Effect::InitializeRuntime]
        }
    );
    token(&policy.initialized())
}
fn main(policy: &mut PrerenderPolicy, token: u64, response: Response) -> PolicyResult {
    assert_eq!(
        policy.main_head(token, &head(&response)).status,
        Status::Ready
    );
    policy.save_main(token, response)
}
fn finish_visit(policy: &mut PrerenderPolicy, token: u64, current: &ResponseHead) -> PolicyResult {
    assert_eq!(policy.dependencies_done(token).status, Status::Ready);
    assert_eq!(policy.crawl_main(token, current).status, Status::Ready);
    policy.complete_visit(token)
}

#[test]
fn real_header_bytes_survive_save_and_publication_follows_manifest_alias() {
    let mut policy = make_policy(config("3.0.1"));
    let token = started(&mut policy);
    let mut body = response(
        200,
        &[
            ("content-type", "text/html"),
            ("set-cookie", "a=1"),
            ("set-cookie", "b=2"),
        ],
        b"<p>hello</p>",
    );
    body.headers.push((b"x-byte".to_vec(), vec![0x80, 0xff]));
    assert_eq!(body.normalized_headers().unwrap()["x-byte"], "\u{80}ÿ");
    assert_eq!(body.normalized_headers().unwrap()["set-cookie"], "b=2");
    let result = main(&mut policy, token, body.clone());
    let kinds: Vec<_> = result
        .effects
        .iter()
        .map(|effect| match effect {
            Effect::Write { .. } => "write",
            Effect::ManifestAsset { .. } => "manifest",
            Effect::SavedAlias { .. } => "alias",
            Effect::Publish { .. } => "publish",
            _ => "metadata",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "write", "metadata", "metadata", "manifest", "alias", "publish"
        ]
    );
    assert_eq!(policy.artifacts()["pages/index.html"], body.body);
    assert_eq!(
        finish_visit(&mut policy, token, &head(&body)).status,
        Status::Ready
    );
    assert_eq!(policy.finish().status, Status::Finished);
}

#[test]
fn redirect_starts_target_before_output_write_and_never_publishes_redirect_alias() {
    let mut config = config("3.0.1");
    config.queue.concurrency = 2;
    let mut policy = make_policy(config);
    let initial = started(&mut policy);
    let redirected = response(302, &[("location", "/target")], b"");
    let result = main(&mut policy, initial, redirected.clone());
    assert!(
        matches!(&result.effects[0], Effect::StartVisit { request } if request.decoded == "/target")
    );
    assert!(matches!(&result.effects[1], Effect::Write { path, .. } if path == "pages/index.html"));
    assert!(!result.effects.iter().any(|e| matches!(
        e,
        Effect::Publish { .. } | Effect::ManifestAsset { .. } | Effect::SavedAlias { .. }
    )));
    let target = token(&result);
    finish_visit(&mut policy, initial, &head(&redirected));
    let body = response(200, &[("content-type", "text/html")], b"target");
    main(&mut policy, target, body.clone());
    finish_visit(&mut policy, target, &head(&body));
    assert_eq!(policy.finish().status, Status::Finished);
}

#[test]
fn dependencies_update_prerender_map_before_save_and_crawl_only_after_iteration() {
    let mut policy = make_policy(config("3.0.1"));
    let visit = started(&mut policy);
    let body = response(
        200,
        &[("content-type", "text/html")],
        b"<a href='/next?q=ignored#ok'>next</a>",
    );
    main(&mut policy, visit, body.clone());
    let dependency = Dependency {
        path: "/data".into(),
        encoded_path: "/data".into(),
        decoded_path: "/data".into(),
        response: response(
            200,
            &[
                ("content-type", "application/json"),
                ("x-sveltekit-prerender", "true"),
                ("x-sveltekit-routeid", "/data"),
            ],
            b"{}",
        ),
    };
    let dep_head = DependencyHead {
        path: dependency.path.clone(),
        encoded_path: dependency.encoded_path.clone(),
        decoded_path: dependency.decoded_path.clone(),
        response: head(&dependency.response),
    };
    assert_eq!(
        policy.dependency_head(visit, &dep_head).status,
        Status::Ready
    );
    assert_eq!(
        policy.queue_snapshot().prerender_map.last(),
        Some(&("/data".into(), PrerenderValue::True))
    );
    let result = policy.save_dependency(visit, dependency);
    assert!(
        result.effects.iter().any(
            |e| matches!(e, Effect::Publish { file, bytes } if file == "data" && bytes == b"{}")
        )
    );
    assert!(!policy.queue_snapshot().seen.contains(&"/next".into()));
    let next = token(&finish_visit(&mut policy, visit, &head(&body)));
    assert_eq!(
        policy.queue_snapshot().expected_hashlinks,
        vec![("/next#ok".into(), vec!["/".into()])]
    );
    let body = response(
        200,
        &[("content-type", "text/html")],
        b"<p id='ok'>present</p>",
    );
    main(&mut policy, next, body.clone());
    finish_visit(&mut policy, next, &head(&body));
    assert_eq!(policy.finish().status, Status::Finished);
}

#[test]
fn early_crawl_is_a_real_failure_not_a_partial_success() {
    let mut policy = make_policy(config("3.0.1"));
    let visit = started(&mut policy);
    let body = response(
        200,
        &[("content-type", "text/html")],
        b"<a href='/late'>link</a>",
    );
    main(&mut policy, visit, body.clone());
    assert!(matches!(
        policy.crawl_main(visit, &head(&body)).status,
        Status::Failed { .. }
    ));
    assert!(!policy.queue_snapshot().seen.contains(&"/late".into()));
}

#[test]
fn final_live_headers_control_crawl_instead_of_stale_main_snapshot() {
    let mut modern_config = config("3.0.1");
    modern_config.queue.entries = vec!["/page".into()];
    let mut policy = make_policy(modern_config);
    let visit = started(&mut policy);
    let body = response(
        200,
        &[("content-type", "text/plain")],
        b"<a href='/next'>next</a>",
    );
    main(&mut policy, visit, body.clone());
    policy.dependencies_done(visit);
    let current = head(&response(
        200,
        &[("content-type", "TEXT/HTML; charset=utf-8")],
        b"",
    ));
    assert_eq!(policy.crawl_main(visit, &current).status, Status::Ready);
    assert!(policy.queue_snapshot().seen.contains(&"/next".into()));
    let mut legacy_config = config("2.69.2");
    legacy_config.queue.entries = vec!["/page".into()];
    let mut legacy = make_policy(legacy_config);
    let visit = started(&mut legacy);
    main(&mut legacy, visit, body);
    legacy.dependencies_done(visit);
    assert_eq!(legacy.crawl_main(visit, &current).status, Status::Ready);
    assert!(!legacy.queue_snapshot().seen.contains(&"/next".into()));
}

#[test]
fn custom_http_handler_resumes_exact_save_before_dependencies() {
    let mut config = config("3.0.1");
    config.save.http_error = HttpErrorPolicy::Custom {
        name: "handle".into(),
    };
    let mut policy = make_policy(config);
    let visit = started(&mut policy);
    let body = response(404, &[], b"missing");
    let result = main(&mut policy, visit, body.clone());
    let Status::AwaitingHandler { handle } = result.status else {
        panic!("expected actual callback request")
    };
    assert_eq!(handle.source, HandlerSource::Save);
    assert!(policy.save_snapshot().saved.is_empty());
    let resumed = policy.resume_handler(handle, Ok(()));
    assert_eq!(resumed.status, Status::Ready);
    assert!(
        resumed
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SavedAlias { file, .. } if file == "index.html"))
    );
    assert!(
        !resumed
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Publish { .. }))
    );
    assert!(policy.artifacts().is_empty());
    finish_visit(&mut policy, visit, &head(&body));
    assert_eq!(policy.finish().status, Status::Finished);
}

#[test]
fn missing_hashlink_callback_and_unseen_route_callback_preserve_finish_order() {
    let mut config = config("3.0.1");
    config.queue.handle_missing_id = HandlerPolicy::Custom {
        name: "missing".into(),
    };
    config.queue.handle_unseen_routes = HandlerPolicy::Custom {
        name: "unseen".into(),
    };
    let routes = vec![RouteMetadata {
        id: "/required".into(),
        prerender: Some(PrerenderValue::True),
        entries: None,
    }];
    let mut policy =
        PrerenderPolicy::new(config, routes, AssetSnapshot::default(), BTreeMap::new()).unwrap();
    let visit = started(&mut policy);
    let body = response(
        200,
        &[("content-type", "text/html")],
        b"<a href='#missing'>missing</a>",
    );
    main(&mut policy, visit, body.clone());
    finish_visit(&mut policy, visit, &head(&body));
    let first = policy.finish();
    let Status::AwaitingHandler { handle } = first.status else {
        panic!()
    };
    assert!(matches!(
        &first.effects[0],
        Effect::QueueHandler {
            diagnostic: queue::QueueDiagnostic::MissingId { .. },
            ..
        }
    ));
    let second = policy.resume_handler(handle, Ok(()));
    let Status::AwaitingHandler { handle } = second.status else {
        panic!()
    };
    assert!(matches!(
        &second.effects[0],
        Effect::QueueHandler {
            diagnostic: queue::QueueDiagnostic::UnseenRoutes { .. },
            ..
        }
    ));
    assert_eq!(
        policy.resume_handler(handle, Ok(())).status,
        Status::Finished
    );
}

#[test]
fn dependency_duplicate_does_not_republish_or_record_new_route() {
    let mut policy = make_policy(config("3.0.1"));
    let visit = started(&mut policy);
    let body = response(200, &[("content-type", "text/html")], b"main");
    main(&mut policy, visit, body.clone());
    let dependency = Dependency {
        path: "/index.html".into(),
        encoded_path: "/index.html".into(),
        decoded_path: "/index.html".into(),
        response: response(200, &[("x-sveltekit-routeid", "/duplicate")], b"different"),
    };
    policy.dependency_head(
        visit,
        &DependencyHead {
            path: dependency.path.clone(),
            encoded_path: dependency.encoded_path.clone(),
            decoded_path: dependency.decoded_path.clone(),
            response: head(&dependency.response),
        },
    );
    assert!(policy.save_dependency(visit, dependency).effects.is_empty());
    assert!(
        !policy
            .queue_snapshot()
            .prerendered_routes
            .contains(&"/duplicate".into())
    );
    assert_eq!(policy.artifacts().len(), 1);
}

#[test]
fn default_http_error_and_outside_base_never_synthesize_success() {
    let mut policy = make_policy(config("3.0.1"));
    let visit = started(&mut policy);
    assert!(matches!(
        main(&mut policy, visit, response(500, &[], b"error")).status,
        Status::Failed { .. }
    ));
    assert!(policy.artifacts().is_empty());
    let mut config = config("3.0.1");
    config.queue.base = "/base".into();
    config.save.base = "/base".into();
    config.queue.concurrency = 2;
    let mut other = make_policy(config);
    let visit = started(&mut other);
    let result = main(
        &mut other,
        visit,
        response(302, &[("location", "/outside")], b""),
    );
    let Status::Failed { message } = result.status else {
        panic!()
    };
    assert!(message.contains("prerender_path_outside_base"));
    assert!(other.artifacts().is_empty());
}

#[test]
fn external_origin_and_known_assets_are_not_started_by_crawl() {
    let mut config = config("3.0.1");
    config.queue.concurrency = 4;
    let mut policy = PrerenderPolicy::new(
        config,
        vec![],
        AssetSnapshot {
            client_files: vec!["asset.png".into()],
            server_immutable_files: vec![],
        },
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(policy.start(true).status, Status::Initializing);
    let visit = token(&policy.initialized());
    let body = response(200, &[("content-type", "text/html")], b"<img src='/asset.png'><a href='https://outside/x'>x</a><a href='http://sveltekit-prerender/new'>new</a><a href='//outside/z'>z</a>");
    main(&mut policy, visit, body.clone());
    policy.dependencies_done(visit);
    let result = policy.crawl_main(visit, &head(&body));
    let starts: Vec<_> = result
        .effects
        .iter()
        .filter_map(|e| {
            if let Effect::StartVisit { request } = e {
                Some(request.decoded.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(starts, vec!["/new"]);
    assert!(policy.queue_snapshot().seen.contains(&"/asset.png".into()));
}

#[test]
fn invalid_url_policy_warns_or_resumes_custom_callback_before_links() {
    for custom in [false, true] {
        let mut config = config("3.0.1");
        config.queue.concurrency = 2;
        config.invalid_url = if custom {
            HandlerPolicy::Custom {
                name: "invalid".into(),
            }
        } else {
            HandlerPolicy::Warn
        };
        let mut policy = make_policy(config);
        let visit = started(&mut policy);
        let body = response(
            200,
            &[("content-type", "text/html")],
            b"<a href='http://[broken'>bad</a><a href='/next'>next</a>",
        );
        main(&mut policy, visit, body.clone());
        policy.dependencies_done(visit);
        let result = policy.crawl_main(visit, &head(&body));
        if custom {
            let Status::AwaitingHandler { handle } = result.status else {
                panic!()
            };
            assert_eq!(handle.source, HandlerSource::InvalidUrl);
            assert!(!policy.queue_snapshot().seen.contains(&"/next".into()));
            let resumed = policy.resume_handler(handle, Ok(()));
            assert_eq!(resumed.status, Status::Ready);
            assert!(matches!(&resumed.effects[..], [Effect::StartVisit { .. }]));
        } else {
            assert_eq!(result.status, Status::Ready);
            assert!(matches!(
                &result.effects[..],
                [Effect::Warning { .. }, Effect::StartVisit { .. }]
            ));
        }
    }
}

#[test]
fn entry_mismatch_handler_precedes_any_main_body_save() {
    let mut config = config("3.0.1");
    config.queue.entries.clear();
    config.queue.handle_entry_generator_mismatch = HandlerPolicy::Custom {
        name: "mismatch".into(),
    };
    config.queue.handle_unseen_routes = HandlerPolicy::Ignore;
    let routes = vec![RouteMetadata {
        id: "/[id]".into(),
        prerender: Some(PrerenderValue::True),
        entries: Some(vec!["/actual".into()]),
    }];
    let mut policy =
        PrerenderPolicy::new(config, routes, AssetSnapshot::default(), BTreeMap::new()).unwrap();
    let visit = started(&mut policy);
    let body = response(
        200,
        &[
            ("content-type", "text/html"),
            ("x-sveltekit-routeid", "/actual"),
        ],
        b"<p>body</p>",
    );
    let result = policy.main_head(visit, &head(&body));
    let Status::AwaitingHandler { handle } = result.status else {
        panic!()
    };
    assert_eq!(handle.source, HandlerSource::Queue);
    assert!(policy.artifacts().is_empty());
    assert_eq!(policy.resume_handler(handle, Ok(())).status, Status::Ready);
    assert_eq!(policy.save_main(visit, body.clone()).status, Status::Ready);
    finish_visit(&mut policy, visit, &head(&body));
    assert_eq!(policy.finish().status, Status::Finished);
}

fn outside_config(policy: HttpErrorPolicy) -> PolicyConfig {
    let mut config = config("3.0.1");
    config.queue.base = "/base".into();
    config.save.base = "/base".into();
    config.queue.entries.clear();
    config.queue.concurrency = 4;
    config.save.http_error = policy;
    config
}
#[test]
fn outside_warn_ignore_retain_queue_slot_until_scheduler_acknowledges() {
    for http in [HttpErrorPolicy::Warn, HttpErrorPolicy::Ignore] {
        let warn = matches!(http, HttpErrorPolicy::Warn);
        let mut policy = make_policy(outside_config(http));
        assert_eq!(policy.start(false).status, Status::Initializing);
        assert!(policy.initialized().effects.is_empty());
        let result = policy.enqueue(EnqueueInput {
            decoded: "/outside".into(),
            encoded: None,
            referrer: Some("/base/page".into()),
            generated_from_id: None,
        });
        assert_eq!(result.status, Status::Ready);
        assert_eq!(result.effects.len(), if warn { 2 } else { 1 });
        assert!(matches!(
            result.effects.last(),
            Some(Effect::OutsideBaseReturned { .. })
        ));
        assert!(!policy.queue.is_closed());
        let Effect::OutsideBaseReturned { visit_token } = result.effects.last().unwrap() else {
            panic!()
        };
        assert!(matches!(policy.finish().status, Status::Rejected { .. }));
        assert_eq!(
            policy.complete_outside_visit(*visit_token).status,
            Status::Ready
        );
        assert!(policy.queue.is_closed());
        assert!(matches!(
            policy.complete_outside_visit(*visit_token).status,
            Status::Rejected { .. }
        ));
        assert!(policy.artifacts().is_empty());
        assert_eq!(policy.save_snapshot(), &save::SaveSnapshot::default());
        assert_eq!(policy.finish().status, Status::Finished);
    }
}
#[test]
fn outside_custom_preserves_all_reserved_ready_requests_and_effect_order() {
    let mut policy = make_policy(outside_config(HttpErrorPolicy::Custom {
        name: "http".into(),
    }));
    assert_eq!(policy.start(false).status, Status::Initializing);
    // Preload the primitive ready queue before the scheduler's initial drain.
    // Its take_ready reserves all four requests in one call; suspension must
    // retain the remainder, not drop requests already counted as running.
    for path in ["/base/first", "/outside-one", "/outside-two", "/base/last"] {
        policy
            .queue
            .enqueue(EnqueueInput {
                decoded: path.into(),
                encoded: None,
                referrer: None,
                generated_from_id: None,
            })
            .unwrap();
    }
    let result = policy.initialized();
    let first = token(&result);
    let Status::AwaitingHandler { handle: one } = result.status else {
        panic!()
    };
    assert!(
        matches!(&result.effects[..], [Effect::StartVisit { .. }, Effect::HttpHandler { details, .. }] if details.path == "/outside-one")
    );
    let stale = HandlerToken {
        source: one.source,
        token: one.token + 99,
    };
    assert!(matches!(
        policy.resume_handler(stale, Ok(())).status,
        Status::Rejected { .. }
    ));
    assert!(matches!(policy.finish().status, Status::Rejected { .. }));
    let second = policy.resume_handler(one, Ok(()));
    let Status::AwaitingHandler { handle: two } = second.status else {
        panic!()
    };
    assert!(
        matches!(&second.effects[..], [Effect::OutsideBaseReturned { .. }, Effect::HttpHandler { details, .. }] if details.path == "/outside-two")
    );
    let returned_one = match &second.effects[0] {
        Effect::OutsideBaseReturned { visit_token } => *visit_token,
        _ => panic!(),
    };
    // A synchronous callback is still on the host stack; a queue reaction
    // cannot interleave it, even for another visit that has already returned.
    assert!(matches!(
        policy.complete_outside_visit(returned_one).status,
        Status::Rejected { .. }
    ));
    let last = policy.resume_handler(two, Ok(()));
    assert_eq!(last.status, Status::Ready);
    assert!(
        matches!(&last.effects[..], [Effect::OutsideBaseReturned { .. }, Effect::StartVisit { request }] if request.decoded == "/base/last")
    );
    let outside_one = match &second.effects[0] {
        Effect::OutsideBaseReturned { visit_token } => *visit_token,
        _ => panic!(),
    };
    let outside_two = match &last.effects[0] {
        Effect::OutsideBaseReturned { visit_token } => *visit_token,
        _ => panic!(),
    };
    let last = token(&last);
    assert_eq!(
        policy.complete_outside_visit(outside_one).status,
        Status::Ready
    );
    assert_eq!(
        policy.complete_outside_visit(outside_two).status,
        Status::Ready
    );
    assert_eq!(policy.active.len(), 2);
    assert_eq!(policy.save_snapshot(), &save::SaveSnapshot::default());
    for visit in [first, last] {
        let body = response(200, &[("content-type", "text/html")], b"body");
        main(&mut policy, visit, body.clone());
        finish_visit(&mut policy, visit, &head(&body));
    }
    assert_eq!(policy.finish().status, Status::Finished);
}
#[test]
fn outside_callback_resumes_remaining_redirect_write_without_alias_or_publication() {
    let mut config = outside_config(HttpErrorPolicy::Custom {
        name: "http".into(),
    });
    config.queue.entries = vec!["/page".into()];
    let mut policy = make_policy(config);
    let visit = started(&mut policy);
    let body = response(302, &[("location", "/outside")], b"");
    let result = main(&mut policy, visit, body.clone());
    let Status::AwaitingHandler { handle } = result.status else {
        panic!()
    };
    assert!(matches!(&result.effects[..], [Effect::HttpHandler { .. }]));
    assert!(policy.artifacts().is_empty());
    let resumed = policy.resume_handler(handle, Ok(()));
    assert!(matches!(
        &resumed.effects[0],
        Effect::OutsideBaseReturned { .. }
    ));
    assert!(matches!(&resumed.effects[1], Effect::Write { path, .. } if path == "pages/page.html"));
    assert!(!resumed.effects.iter().any(|e| matches!(
        e,
        Effect::ManifestAsset { .. } | Effect::Publish { .. } | Effect::SavedAlias { .. }
    )));
    let Effect::OutsideBaseReturned { visit_token } = &resumed.effects[0] else {
        panic!()
    };
    assert_eq!(
        policy.complete_outside_visit(*visit_token).status,
        Status::Ready
    );
    finish_visit(&mut policy, visit, &head(&body));
    assert_eq!(policy.finish().status, Status::Finished);
}
#[test]
// Explicit current limitation: the shared SaveState poisons on callback throw.
// This tests error preservation, not upstream per-visit rejection scheduling.
fn outside_callback_throw_retains_explicit_save_state_failure_limitation() {
    let mut policy = make_policy(outside_config(HttpErrorPolicy::Custom {
        name: "http".into(),
    }));
    policy.start(false);
    for path in ["/outside", "/base/later"] {
        policy
            .queue
            .enqueue(EnqueueInput {
                decoded: path.into(),
                encoded: None,
                referrer: None,
                generated_from_id: None,
            })
            .unwrap();
    }
    let Status::AwaitingHandler { handle } = policy.initialized().status else {
        panic!()
    };
    let result = policy.resume_handler(handle, Err("same thrown failure".into()));
    assert_eq!(
        result.status,
        Status::Failed {
            message: "same thrown failure".into()
        }
    );
    assert!(result.effects.is_empty());
    assert!(policy.active.is_empty());
    assert_eq!(policy.save_snapshot(), &save::SaveSnapshot::default());
}
#[test]
fn multiple_invalid_url_callbacks_resume_before_ids_and_links_with_stale_protection() {
    let mut config = config("3.0.1");
    config.queue.concurrency = 2;
    config.invalid_url = HandlerPolicy::Custom {
        name: "invalid".into(),
    };
    let mut policy = make_policy(config);
    let visit = started(&mut policy);
    let body = response(200, &[("content-type", "text/html")], b"<div id='here'></div><a href='http://[one'>bad</a><a href='/next'>next</a><a href='http://[two'>bad</a>");
    main(&mut policy, visit, body.clone());
    policy.dependencies_done(visit);
    let first = policy.crawl_main(visit, &head(&body));
    let Status::AwaitingHandler { handle: one } = first.status else {
        panic!()
    };
    assert!(
        matches!(&first.effects[..], [Effect::InvalidUrlHandler { href, .. }] if href == "http://[one")
    );
    assert!(policy.queue_snapshot().actual_hashlinks.is_empty());
    assert!(matches!(
        policy
            .resume_handler(
                HandlerToken {
                    source: HandlerSource::Save,
                    token: one.token
                },
                Ok(())
            )
            .status,
        Status::Rejected { .. }
    ));
    let second = policy.resume_handler(one, Ok(()));
    let Status::AwaitingHandler { handle: two } = second.status else {
        panic!()
    };
    assert_ne!(one, two);
    assert!(
        matches!(&second.effects[..], [Effect::InvalidUrlHandler { href, .. }] if href == "http://[two")
    );
    assert!(policy.queue_snapshot().actual_hashlinks.is_empty());
    assert!(!policy.queue_snapshot().seen.contains(&"/next".into()));
    let result = policy.resume_handler(two, Ok(()));
    assert_eq!(result.status, Status::Ready);
    assert!(
        matches!(&result.effects[..], [Effect::StartVisit { request }] if request.decoded == "/next")
    );
    assert_eq!(
        policy.queue_snapshot().actual_hashlinks,
        vec![("/".into(), vec!["here".into()])]
    );
    assert_eq!(policy.active[&visit].phase, Phase::Complete);
}
#[test]
fn invalid_url_default_ignore_and_callback_failure_have_explicit_results() {
    for mode in [0, 1, 2] {
        let mut config = config("3.0.1");
        config.queue.concurrency = 2;
        config.invalid_url = match mode {
            0 => HandlerPolicy::Default,
            1 => HandlerPolicy::Ignore,
            _ => HandlerPolicy::Custom {
                name: "invalid".into(),
            },
        };
        let mut policy = make_policy(config);
        let visit = started(&mut policy);
        let body = response(
            200,
            &[("content-type", "text/html")],
            b"<p id='yes'></p><a href='http://[bad'>bad</a><a href='/next'>next</a>",
        );
        main(&mut policy, visit, body.clone());
        policy.dependencies_done(visit);
        let result = policy.crawl_main(visit, &head(&body));
        match mode {
            0 => {
                let Status::Failed { message } = result.status else {
                    panic!()
                };
                assert!(message.contains("implement `handleInvalidUrl`"));
            }
            1 => {
                assert_eq!(result.status, Status::Ready);
                assert!(matches!(&result.effects[..], [Effect::StartVisit { .. }]));
            }
            _ => {
                let Status::AwaitingHandler { handle } = result.status else {
                    panic!()
                };
                assert_eq!(
                    policy
                        .resume_handler(handle, Err("invalid handler throw".into()))
                        .status,
                    Status::Failed {
                        message: "invalid handler throw".into()
                    }
                );
            }
        }
        if mode != 1 {
            assert!(policy.queue_snapshot().actual_hashlinks.is_empty());
            assert!(!policy.queue_snapshot().seen.contains(&"/next".into()));
        }
    }
}

#[test]
fn outside_deferred_release_preserves_seed_fifo_and_remote_producer_continuation() {
    for version in ["2.69.2", "3.0.1"] {
        let mut config = outside_config(HttpErrorPolicy::Ignore);
        config.save.kit_version = version.into();
        config.queue.kit_version = version.into();
        config.queue.concurrency = 1;
        config.queue.entries = vec!["/first".into(), "/second".into()];
        let mut policy = make_policy(config);
        assert_eq!(policy.start(true).status, Status::Initializing);
        let first = token(&policy.initialized());
        for path in ["/outside", "/base/after-outside"] {
            let result = policy.enqueue(EnqueueInput {
                decoded: path.into(),
                encoded: None,
                referrer: None,
                generated_from_id: None,
            });
            assert_eq!(result.status, Status::Ready);
            assert!(result.effects.is_empty());
        }
        let body = response(200, &[("content-type", "text/plain")], b"body");
        main(&mut policy, first, body.clone());
        let second = finish_visit(&mut policy, first, &head(&body));
        assert!(
            matches!(&second.effects[..], [Effect::StartVisit { request }] if request.decoded == "/base/second")
        );
        let second = token(&second);
        main(&mut policy, second, body.clone());
        let returned = finish_visit(&mut policy, second, &head(&body));
        let [Effect::OutsideBaseReturned { visit_token }] = &returned.effects[..] else {
            panic!("{returned:?}")
        };
        let outside = *visit_token;
        assert!(!policy.queue.is_closed());
        assert!(matches!(policy.finish().status, Status::Rejected { .. }));
        assert!(matches!(
            policy.complete_outside_visit(outside + 100).status,
            Status::Rejected { .. }
        ));
        // This represents an actual remote inputs await continuation before the
        // queue's second reaction. Enqueuing cannot steal the still-held slot.
        let remote = policy.seed_remote(RemoteSeed {
            id: "module/export".into(),
            has_arg: false,
            serialized_arguments: vec![],
        });
        assert_eq!(remote.status, Status::Ready);
        assert!(remote.effects.is_empty());
        let next = policy.complete_outside_visit(outside);
        assert!(
            matches!(&next.effects[..], [Effect::StartVisit { request }] if request.decoded == "/base/after-outside")
        );
        assert!(matches!(
            policy.complete_outside_visit(outside).status,
            Status::Rejected { .. }
        ));
        let next = token(&next);
        main(&mut policy, next, body.clone());
        let remote = finish_visit(&mut policy, next, &head(&body));
        assert!(
            matches!(&remote.effects[..], [Effect::StartVisit { request }] if request.decoded == "/base/_app/remote/module/export")
        );
        let remote = token(&remote);
        main(&mut policy, remote, body.clone());
        assert!(
            finish_visit(&mut policy, remote, &head(&body))
                .effects
                .is_empty()
        );
        assert_eq!(policy.finish().status, Status::Finished);
    }
}

#[test]
fn unresolved_outside_ack_preserves_custom_handler_and_later_queued_visit() {
    let mut config = outside_config(HttpErrorPolicy::Custom {
        name: "http".into(),
    });
    config.queue.concurrency = 1;
    let mut policy = make_policy(config);
    policy.start(false);
    policy.initialized();
    let result = policy.enqueue(EnqueueInput {
        decoded: "/outside".into(),
        encoded: None,
        referrer: None,
        generated_from_id: None,
    });
    let Status::AwaitingHandler { handle } = result.status else {
        panic!()
    };
    // No OutsideBaseReturned event has occurred. Even the correct visit token
    // (1 in this empty queue) cannot be acknowledged before callback return.
    assert!(matches!(
        policy.complete_outside_visit(1).status,
        Status::Rejected { .. }
    ));
    assert!(matches!(policy.finish().status, Status::Rejected { .. }));
    let resumed = policy.resume_handler(handle, Ok(()));
    assert_eq!(resumed.status, Status::Ready);
    let [Effect::OutsideBaseReturned { visit_token }] = &resumed.effects[..] else {
        panic!()
    };
    let outside = *visit_token;
    let queued = policy.enqueue(EnqueueInput {
        decoded: "/base/next".into(),
        encoded: None,
        referrer: None,
        generated_from_id: None,
    });
    assert_eq!(queued.status, Status::Ready);
    assert!(queued.effects.is_empty());
    let next = policy.complete_outside_visit(outside);
    assert!(
        matches!(&next.effects[..], [Effect::StartVisit { request }] if request.decoded == "/base/next")
    );
    let next = token(&next);
    let body = response(200, &[("content-type", "text/plain")], b"done");
    main(&mut policy, next, body.clone());
    finish_visit(&mut policy, next, &head(&body));
    assert_eq!(policy.finish().status, Status::Finished);
}

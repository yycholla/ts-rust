use super::*;

fn state(version: &str, policy: HttpErrorPolicy) -> SaveState {
    SaveState::new(
        SaveConfig {
            kit_version: version.into(),
            base: "".into(),
            origin: "http://sveltekit-prerender".into(),
            http_error: policy,
        },
        BTreeMap::new(),
    )
    .unwrap()
}
fn input(path: &str, status: u16, content_type: Option<&str>) -> SaveInput {
    let mut headers = BTreeMap::new();
    if let Some(value) = content_type {
        headers.insert("content-type".into(), value.into());
    }
    SaveInput {
        decoded: path.into(),
        encoded: path.into(),
        referrer: Some("/from".into()),
        reference_type: ReferenceType::Linked,
        category: Category::Pages,
        status,
        headers,
        body: b"body".to_vec(),
    }
}
fn write(result: &SaveResult) -> (&str, &[u8]) {
    result
        .effects
        .iter()
        .find_map(|effect| {
            if let SaveEffect::Write { path, body } = effect {
                Some((path.as_str(), body.as_slice()))
            } else {
                None
            }
        })
        .unwrap()
}

#[test]
fn filename_uses_utf16_base_length_and_preserves_extensions() {
    for (base, path, html, expected) in [
        ("", "/", true, "index.html"),
        ("/😀", "/😀/🌐", true, "🌐.html"),
        ("", "/page/", true, "page/index.html"),
        ("", "/page.html", true, "page.html"),
        ("", "/page.HTML", true, "page.HTML.html"),
        ("", "/file.json", false, "file.json"),
        ("/long", "/", false, "index.html"),
    ] {
        assert_eq!(output_filename(base, path, html).unwrap(), expected);
    }
    assert!(
        output_filename("x", "/😀", false)
            .unwrap_err()
            .contains("non-scalar")
    );
}

#[test]
fn page_effects_have_exact_order_and_body_bytes() {
    let mut state = state("2.69.2", HttpErrorPolicy::Fail);
    let mut response = input("/foo", 200, Some("text/html"));
    response
        .headers
        .insert("x-sveltekit-routeid".into(), "/%F0%9F%98%80/[x%2fy]".into());
    response.body = vec![0, 255, 10];
    let saved = state.save(response).unwrap();
    assert_eq!(
        saved.effects,
        vec![
            SaveEffect::RouteSeen {
                id: "/😀/[x%2fy]".into()
            },
            SaveEffect::Write {
                path: "pages/foo.html".into(),
                body: vec![0, 255, 10]
            },
            SaveEffect::Page {
                path: "/foo".into(),
                file: "foo.html".into()
            },
            SaveEffect::Path {
                path: "/foo".into()
            },
            SaveEffect::ManifestAsset {
                file: "foo.html".into()
            },
            SaveEffect::Saved {
                file: "foo.html".into(),
                destination: "pages/foo.html".into()
            },
        ]
    );
    assert_eq!(state.inventory().get("pages"), Some(&OutputKind::Directory));
}

#[test]
fn html_media_type_is_versioned_for_all_registered_profiles() {
    for version in [
        "2.67.0",
        "2.68.0",
        "2.69.2",
        "2.69.3",
        "2.70.3",
        "3.0.0-next.32",
        "3.0.1",
    ] {
        let mut state = state(version, HttpErrorPolicy::Fail);
        let result = state
            .save(input("/doc", 200, Some("TEXT/HTML ; charset=utf-8")))
            .unwrap();
        if version.starts_with('2') {
            assert_eq!(write(&result).0, "pages/doc");
            assert_eq!(
                state.snapshot().assets,
                vec![("/doc".into(), Some("TEXT/HTML ; charset=utf-8".into()))]
            );
        } else {
            assert_eq!(write(&result).0, "pages/doc.html");
        }
    }
}

#[test]
fn root_non_html_fails_even_with_ignore_http_policy() {
    for version in ["2.69.2", "3.0.1"] {
        let mut state = state(version, HttpErrorPolicy::Ignore);
        let result = state
            .save(input("/", 200, Some("application/json")))
            .unwrap();
        assert!(matches!(result.status, SaveStatus::Failed { .. }));
        assert!(result.effects.is_empty());
        assert!(state.save(input("/next", 200, None)).is_err());
    }
    let mut state = state("3.0.1", HttpErrorPolicy::Fail);
    assert_eq!(
        write(
            &state
                .save(input("/", 200, Some("text/html;charset=utf-8")))
                .unwrap()
        )
        .0,
        "pages/index.html"
    );
}

#[test]
fn dedup_spans_categories_and_precedes_route_decode() {
    let mut state = state("3.0.1", HttpErrorPolicy::Fail);
    state.save(input("/same", 200, None)).unwrap();
    let mut duplicate = input("/same", 301, None);
    // Redirect's HTML filename differs; first use actual same filename.
    duplicate.status = 200;
    duplicate.category = Category::Data;
    duplicate
        .headers
        .insert("x-sveltekit-routeid".into(), "%broken".into());
    let result = state.save(duplicate).unwrap();
    assert_eq!(
        result.status,
        SaveStatus::Duplicate {
            file: "same".into()
        }
    );
    assert!(result.effects.is_empty());
    assert_eq!(state.snapshot().written, vec!["same"]);
    assert_eq!(
        state.snapshot().saved,
        vec![("same".into(), "pages/same".into())]
    );
}

#[test]
fn redirect_enqueues_before_write_and_skips_manifest_aliases() {
    let mut state = state("3.0.1", HttpErrorPolicy::Fail);
    let mut response = input("/a/b", 307, None);
    response
        .headers
        .insert("location".into(), "../c?q=1#x".into());
    response
        .headers
        .insert("x-sveltekit-routeid".into(), "/a/b".into());
    let result = state.save(response).unwrap();
    assert_eq!(
        result.effects[0],
        SaveEffect::RouteSeen { id: "/a/b".into() }
    );
    assert_eq!(
        result.effects[1],
        SaveEffect::Enqueue {
            referrer: "/a/b".into(),
            decoded: "/c?q=1#x".into(),
            encoded: "/c?q=1#x".into()
        }
    );
    assert_eq!(write(&result), ("pages/a/b.html", b"<script>location.href=\"../c?q=1#x\";</script><meta http-equiv=\"refresh\" content=\"0;url=../c?q=1#x\">".as_slice()));
    assert_eq!(
        state.snapshot().redirects,
        vec![("/a/b".into(), (307, "/c?q=1#x".into()))]
    );
    assert!(state.snapshot().manifest_assets.is_empty());
    assert!(state.snapshot().saved.is_empty());
}

#[test]
fn redirect_location_script_and_attribute_escaping_are_independent() {
    let location = "</script>\"&\\\u{2028}\u{2029}\u{1}";
    let html = redirect_html(location);
    assert!(html.starts_with(
        "<script>location.href=\"\\u003C/script>\\\"&\\\\\\u2028\\u2029\\u0001\";</script>"
    ));
    assert!(html.contains("content=\"0;url=</script>&quot;&amp;\\"));
    assert_eq!(html.matches("</script>").count(), 2); // literal only in the inert attribute and generated closing tag
}

#[test]
fn normalized_external_missing_and_empty_redirects() {
    let mut state = state("2.69.2", HttpErrorPolicy::Fail);
    for (path, location, normalize, enqueue) in [
        ("/a", "/target", Some("false"), true), // any nonempty header is truthy
        ("/b", "//example.org/x", None, false),
        ("/c", "https://example.org/x", None, false),
    ] {
        let mut response = input(path, 302, None);
        response.headers.insert("location".into(), location.into());
        if let Some(value) = normalize {
            response
                .headers
                .insert("x-sveltekit-normalize".into(), value.into());
        }
        let result = state.save(response).unwrap();
        assert_eq!(
            result
                .effects
                .iter()
                .any(|e| matches!(e, SaveEffect::Enqueue { .. })),
            enqueue
        );
        assert_eq!(
            result
                .effects
                .iter()
                .any(|e| matches!(e, SaveEffect::Write { .. })),
            normalize.is_none()
        );
    }
    for path in ["/missing", "/empty"] {
        let mut response = input(path, 304, None);
        if path == "/empty" {
            response.headers.insert("location".into(), "".into());
        }
        let result = state.save(response).unwrap();
        assert!(matches!(&result.effects[..], [SaveEffect::Warning { .. }]));
    }
    assert!(!state.snapshot().written.contains(&"a.html".into()));
}

#[test]
fn non200_success_does_not_write_but_registers_aliases() {
    let mut state = state("3.0.1", HttpErrorPolicy::Fail);
    for status in [201, 204, 206, 299] {
        let result = state
            .save(input("/empty", status, Some("text/html")))
            .unwrap();
        assert_eq!(
            result.effects,
            vec![
                SaveEffect::ManifestAsset {
                    file: "empty.html".into()
                },
                SaveEffect::Saved {
                    file: "empty.html".into(),
                    destination: "pages/empty.html".into()
                }
            ]
        );
    }
    assert!(state.snapshot().written.is_empty());
    assert!(state.snapshot().paths.is_empty());
    assert_eq!(state.snapshot().manifest_assets, vec!["empty.html"]);
}

#[test]
fn ignored_errors_record_route_and_nonwriting_alias_including_trailing_slash() {
    let mut state = state("3.0.1", HttpErrorPolicy::Ignore);
    let mut response = input("/missing/", 404, None);
    response.category = Category::Dependencies;
    response
        .headers
        .insert("x-sveltekit-routeid".into(), "/missing".into());
    let result = state.save(response).unwrap();
    assert_eq!(result.effects.len(), 3);
    assert!(matches!(&result.effects[0], SaveEffect::RouteSeen { .. }));
    assert_eq!(
        state.snapshot().saved,
        vec![("missing/".into(), "dependencies/missing/".into())]
    );
    assert!(state.inventory().is_empty());
    assert!(state.snapshot().paths.is_empty());
}

#[test]
fn warning_failure_default_and_raw_zero_status() {
    for version in ["2.69.2", "3.0.1"] {
        let mut warn = state(version, HttpErrorPolicy::Warn);
        let result = warn.save(input("/bad", 0, None)).unwrap();
        assert!(matches!(&result.effects[0], SaveEffect::Warning { .. }));
        assert_eq!(warn.snapshot().saved.len(), 1);
        let mut fail = state(version, HttpErrorPolicy::Fail);
        assert!(matches!(
            fail.save(input("/bad", 500, None)).unwrap().status,
            SaveStatus::Failed { .. }
        ));
        assert!(fail.snapshot().saved.is_empty());
        let mut default = state(version, HttpErrorPolicy::Default);
        let SaveStatus::Failed { message } = default.save(input("/bad", 404, None)).unwrap().status
        else {
            panic!("expected failure")
        };
        assert!(message.contains("implement `handleHttpError`"));
    }
}

#[test]
fn custom_handler_suspends_with_exact_details_and_resumes_once() {
    let mut state = state(
        "2.69.2",
        HttpErrorPolicy::Custom {
            name: "myHandler".into(),
        },
    );
    let mut response = input("/bad", 418, None);
    response.reference_type = ReferenceType::Fetched;
    let result = state.save(response).unwrap();
    let SaveStatus::AwaitingHttpHandler { token } = result.status else {
        panic!("expected suspension")
    };
    assert_eq!(
        result.effects,
        vec![SaveEffect::HttpHandler {
            token,
            name: "myHandler".into(),
            details: HttpErrorDetails {
                status: 418,
                path: "/bad".into(),
                referrer: Some("/from".into()),
                reference_type: ReferenceType::Fetched,
                message: "418 /bad (fetched from /from)".into()
            }
        }]
    );
    assert!(state.snapshot().saved.is_empty());
    assert!(state.save(input("/next", 200, None)).is_err());
    assert!(state.resume_http(token + 1, Ok(())).is_err());
    let resumed = state.resume_http(token, Ok(())).unwrap();
    assert_eq!(resumed.effects.len(), 2);
    assert_eq!(state.snapshot().saved.len(), 1);
    assert!(state.resume_http(token, Ok(())).is_err());
}

#[test]
fn throwing_custom_handler_keeps_prior_route_effect_but_no_alias() {
    let mut state = state(
        "3.0.1",
        HttpErrorPolicy::Custom {
            name: "handler".into(),
        },
    );
    let mut response = input("/bad", 500, None);
    response
        .headers
        .insert("x-sveltekit-routeid".into(), "/bad".into());
    let SaveStatus::AwaitingHttpHandler { token } = state.save(response).unwrap().status else {
        panic!()
    };
    assert_eq!(
        state
            .resume_http(token, Err("callback identity diagnostic".into()))
            .unwrap()
            .status,
        SaveStatus::Failed {
            message: "callback identity diagnostic".into()
        }
    );
    assert_eq!(state.snapshot().prerendered_routes, vec!["/bad"]);
    assert!(state.snapshot().saved.is_empty());
}

#[test]
fn output_inventory_detects_both_collisions_and_allows_file_overwrite() {
    let config = SaveConfig {
        kit_version: "3.0.1".into(),
        base: "".into(),
        origin: "http://local".into(),
        http_error: HttpErrorPolicy::Fail,
    };
    for (path, kind, request) in [
        ("pages/a", OutputKind::Directory, "/a"),
        ("pages/a", OutputKind::File, "/a/b"),
    ] {
        let mut state =
            SaveState::new(config.clone(), BTreeMap::from([(path.into(), kind)])).unwrap();
        let SaveStatus::Failed { message } = state.save(input(request, 200, None)).unwrap().status
        else {
            panic!("expected collision");
        };
        assert!(message.contains("prerender_directory_conflict"));
        assert!(state.snapshot().written.is_empty());
    }
    let mut state = SaveState::new(
        config,
        BTreeMap::from([("pages/a".into(), OutputKind::File)]),
    )
    .unwrap();
    assert_eq!(
        write(&state.save(input("/a", 200, None)).unwrap()).0,
        "pages/a"
    );
}

#[test]
fn invalid_inputs_paths_and_uri_are_never_silently_repaired() {
    for id in ["%", "%x0", "%80", "%C0%80", "%ED%A0%80", "%F4%90%80%80"] {
        assert!(decode_uri(id).is_err());
    }
    assert_eq!(decode_uri("%3f%2F%23%25%C3%A9").unwrap(), "%3f%2F%23%é");
    for path in ["/../escape", "/a/../../escape", "/a\\b", "/a\0b"] {
        let mut state = state("3.0.1", HttpErrorPolicy::Fail);
        let SaveStatus::Failed { message } = state.save(input(path, 200, None)).unwrap().status
        else {
            panic!("expected rejected path");
        };
        assert!(message.contains("unsafe"));
    }
    let mut state = state("3.0.1", HttpErrorPolicy::Fail);
    let mut response = input("/a", 200, None);
    response
        .headers
        .insert("Content-Type".into(), "text/html".into());
    assert!(state.save(response).unwrap_err().contains("normalized"));
}

#[test]
fn failure_keeps_route_and_redirect_schedule_before_write_collision() {
    let config = SaveConfig {
        kit_version: "3.0.1".into(),
        base: "".into(),
        origin: "https://example.org".into(),
        http_error: HttpErrorPolicy::Fail,
    };
    let mut state = SaveState::new(
        config,
        BTreeMap::from([("pages/a.html".into(), OutputKind::Directory)]),
    )
    .unwrap();
    let mut response = input("/a", 301, None);
    response.headers.insert("location".into(), "/target".into());
    response
        .headers
        .insert("x-sveltekit-routeid".into(), "/a".into());
    let result = state.save(response).unwrap();
    assert!(matches!(result.status, SaveStatus::Failed { .. }));
    assert_eq!(
        result.effects,
        vec![
            SaveEffect::RouteSeen { id: "/a".into() },
            SaveEffect::Enqueue {
                referrer: "/a".into(),
                decoded: "/target".into(),
                encoded: "/target".into()
            }
        ]
    );
    assert!(state.snapshot().written.is_empty());
}

#[test]
fn resolved_base_origin_and_error_details_are_not_inferred() {
    let config = SaveConfig {
        kit_version: "3.0.1".into(),
        base: "/😀".into(),
        origin: "https://example.org".into(),
        http_error: HttpErrorPolicy::Custom {
            name: "handler".into(),
        },
    };
    let mut state = SaveState::new(config.clone(), BTreeMap::new()).unwrap();
    let result = state.save(input("/😀/x", 200, None)).unwrap();
    assert_eq!(write(&result).0, "pages/x");
    let result = state.save(input("/missing", 404, None)).unwrap();
    let Some(SaveEffect::HttpHandler { details, .. }) = result.effects.first() else {
        panic!("expected handler");
    };
    assert!(details.message.starts_with("prerender_path_outside_base\n"));
    assert!(details.message.contains("linked from `/from`"));
    assert_eq!(details.path, "/missing");
    let mut invalid = config.clone();
    invalid.kit_version = "3.1.0".into();
    assert!(SaveState::new(invalid, BTreeMap::new()).is_err());
    let mut invalid = config;
    invalid.origin = "https://example.org/path".into();
    assert!(SaveState::new(invalid, BTreeMap::new()).is_err());
}

fn outside() -> HttpErrorInput {
    HttpErrorInput {
        status: 404,
        path: "/outside".into(),
        referrer: Some("/base/page".into()),
        reference_type: ReferenceType::Linked,
    }
}
#[test]
fn standalone_http_policy_never_creates_save_bookkeeping_for_any_handler() {
    for version in ["2.69.2", "3.0.1"] {
        for policy in [
            HttpErrorPolicy::Default,
            HttpErrorPolicy::Fail,
            HttpErrorPolicy::Warn,
            HttpErrorPolicy::Ignore,
        ] {
            let failing = matches!(policy, HttpErrorPolicy::Default | HttpErrorPolicy::Fail);
            let warning = matches!(policy, HttpErrorPolicy::Warn);
            let mut state = SaveState::new(
                SaveConfig {
                    kit_version: version.into(),
                    base: "/base".into(),
                    origin: "http://local".into(),
                    http_error: policy,
                },
                BTreeMap::new(),
            )
            .unwrap();
            let result = state.handle_http_error(outside()).unwrap();
            assert_eq!(matches!(result.status, SaveStatus::Failed { .. }), failing);
            assert_eq!(result.effects.len(), usize::from(warning));
            if let SaveStatus::Failed { message } = result.status {
                assert!(message.contains("does not begin with `base`"));
            }
            assert_eq!(state.snapshot(), &SaveSnapshot::default());
            assert!(state.inventory().is_empty());
        }
    }
}
#[test]
fn standalone_http_custom_continuation_has_no_alias_and_stale_token_preserves_it() {
    for fails in [false, true] {
        let mut state = state(
            "3.0.1",
            HttpErrorPolicy::Custom {
                name: "standalone".into(),
            },
        );
        let result = state.handle_http_error(outside()).unwrap();
        let SaveStatus::AwaitingHttpHandler { token } = result.status else {
            panic!()
        };
        assert!(
            matches!(&result.effects[..], [SaveEffect::HttpHandler { details, .. }] if details.status == 404 && details.path == "/outside")
        );
        assert!(state.resume_http(token + 1, Ok(())).is_err());
        assert!(state.handle_http_error(outside()).is_err());
        let result = state
            .resume_http(
                token,
                if fails {
                    Err("actual synchronous throw".into())
                } else {
                    Ok(())
                },
            )
            .unwrap();
        assert!(result.effects.is_empty());
        assert_eq!(state.snapshot(), &SaveSnapshot::default());
        if fails {
            assert_eq!(
                result.status,
                SaveStatus::Failed {
                    message: "actual synchronous throw".into()
                }
            );
        } else {
            assert_eq!(result.status, SaveStatus::Complete);
            let next = state.handle_http_error(outside()).unwrap();
            assert!(
                matches!(next.status, SaveStatus::AwaitingHttpHandler { token: next } if next > token)
            );
        }
    }
}

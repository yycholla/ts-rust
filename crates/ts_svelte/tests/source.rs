use ts_svelte::{ByteRange, Chunk, Document, Edit, Position, Projection, Range, Snapshot};

fn document(text: &str) -> Document {
    Document::new(Snapshot {
        uri: "file:///Counter.svelte".into(),
        version: 1,
        text: text.into(),
    })
}
fn pos(line: u32, character: u32) -> Position {
    Position { line, character }
}
fn bytes(start: usize, end: usize) -> ByteRange {
    ByteRange { start, end }
}
fn edit(start: Position, end: Position, text: &str) -> Edit {
    Edit {
        range: Some(Range { start, end }),
        text: text.into(),
    }
}

#[test]
fn empty_document_has_one_line_and_an_eof_position() {
    let doc = document("");
    assert_eq!(doc.line_count(), 1);
    assert_eq!(doc.offset_at(pos(0, 0)), Ok(0));
    assert_eq!(doc.position_at(0), Ok(pos(0, 0)));
    assert!(doc.position_at(1).is_err());
}

#[test]
fn ascii_bmp_astral_and_combining_characters_round_trip() {
    let doc = document("aé😀e\u{301}");
    for (offset, character) in [(0, 0), (1, 1), (3, 2), (7, 4), (8, 5), (10, 6)] {
        assert_eq!(doc.offset_at(pos(0, character)), Ok(offset));
        assert_eq!(doc.position_at(offset), Ok(pos(0, character)));
    }
    assert!(doc.offset_at(pos(0, 3)).is_err());
    for offset in [2, 4, 5, 6, 9, 11, usize::MAX] {
        assert!(doc.position_at(offset).is_err());
    }
}

#[test]
fn mixed_line_endings_have_exact_positions_and_trailing_empty_line() {
    let doc = document("a\r\nb\rc\n");
    assert_eq!(doc.line_count(), 4);
    for (offset, position) in [
        (0, pos(0, 0)),
        (1, pos(0, 1)),
        (3, pos(1, 0)),
        (4, pos(1, 1)),
        (5, pos(2, 0)),
        (6, pos(2, 1)),
        (7, pos(3, 0)),
    ] {
        assert_eq!(doc.position_at(offset), Ok(position));
        assert_eq!(doc.offset_at(position), Ok(offset));
    }
    assert!(doc.position_at(2).is_err());
    assert!(doc.offset_at(pos(0, 2)).is_err());
    assert!(doc.offset_at(pos(4, 0)).is_err());
}

#[test]
fn unicode_line_separators_are_content_not_lsp_line_endings() {
    let doc = document("\u{2028}\u{2029}");
    assert_eq!(doc.line_count(), 1);
    assert_eq!(doc.offset_at(pos(0, 2)), Ok(6));
}

#[test]
fn all_representable_boundaries_round_trip_across_many_lines() {
    let text = "a😀é\r\n\r\n中e\u{301}\rfinal\n".repeat(100);
    let doc = document(&text);
    for offset in 0..=text.len() {
        if let Ok(position) = doc.position_at(offset) {
            assert_eq!(doc.offset_at(position), Ok(offset));
        }
    }
}

#[test]
fn edits_use_sequential_coordinates_and_preserve_original() {
    let doc = document("a😀b\r\nc");
    let next = doc
        .edited(
            2,
            &[
                edit(pos(0, 1), pos(0, 3), "xy\n"),
                edit(pos(1, 0), pos(1, 1), "z"),
            ],
        )
        .unwrap();
    assert_eq!(next.text(), "axy\nz\r\nc");
    assert_eq!(next.snapshot().version, 2);
    assert_eq!(doc.text(), "a😀b\r\nc");
    assert_eq!(doc.snapshot().version, 1);
}

#[test]
fn failed_second_edit_does_not_publish_first_edit() {
    let doc = document("abc");
    assert!(
        doc.edited(
            2,
            &[
                edit(pos(0, 0), pos(0, 1), "x"),
                edit(pos(7, 0), pos(7, 0), "y"),
            ]
        )
        .is_err()
    );
    assert_eq!(doc.text(), "abc");
    assert_eq!(doc.snapshot().version, 1);
}

#[test]
fn stale_versions_reversed_ranges_and_surrogate_splits_are_errors() {
    let doc = document("😀a");
    for version in [0, 1] {
        assert_eq!(doc.edited(version, &[]).unwrap_err().code, "stale_version");
    }
    assert!(doc.edited(2, &[edit(pos(0, 2), pos(0, 0), "")]).is_err());
    assert!(doc.edited(2, &[edit(pos(0, 1), pos(0, 2), "")]).is_err());
}

#[test]
fn full_replacement_and_empty_edits_advance_versions() {
    let doc = document("old");
    let next = doc
        .edited(
            3,
            &[Edit {
                range: None,
                text: "new\n".into(),
            }],
        )
        .unwrap();
    assert_eq!(next.text(), "new\n");
    assert_eq!(next.line_count(), 2);
    let again = next.edited(4, &[]).unwrap();
    assert_eq!(again.text(), next.text());
    assert_eq!(again.snapshot().version, 4);
}

#[test]
fn virtual_script_maps_a_real_svelte_error_to_original_utf16_range() {
    let text =
        "<script lang=\"ts\">\nconst face = '😀';\nconst n: number = 'bad';\n</script>\n<p>{n}</p>";
    let doc = document(text);
    let start = text.find("const face").unwrap();
    let end = text.find("</script>").unwrap();
    let prefix = "export {};\n";
    let projection = Projection::new(
        &doc,
        &[
            Chunk::Generated(prefix.into()),
            Chunk::Copy(bytes(start, end)),
        ],
    )
    .unwrap();
    let source_start = text.find("'bad'").unwrap();
    let generated_start = prefix.len() + source_start - start;
    let mapped = projection
        .to_source(bytes(generated_start, generated_start + 5))
        .unwrap()
        .unwrap();
    assert_eq!(mapped, bytes(source_start, source_start + 5));
    assert_eq!(
        doc.editor_range(mapped).unwrap(),
        Range {
            start: pos(2, 18),
            end: pos(2, 23)
        }
    );
    // This is a manually supplied mapping fixture, not a parser or checker.
}

#[test]
fn generated_helpers_and_ranges_crossing_them_are_unmapped() {
    let doc = document("abcdef");
    let projection = Projection::new(
        &doc,
        &[
            Chunk::Copy(bytes(0, 3)),
            Chunk::Generated("__".into()),
            Chunk::Copy(bytes(3, 6)),
        ],
    )
    .unwrap();
    for range in [bytes(3, 4), bytes(1, 7), bytes(3, 3), bytes(5, 5)] {
        assert_eq!(projection.to_source(range).unwrap(), None);
    }
    assert_eq!(
        projection.to_source(bytes(5, 8)).unwrap(),
        Some(bytes(3, 6))
    );
}

#[test]
fn contiguous_copies_coalesce_and_map_cross_segment_ranges() {
    let doc = document("abcdef");
    let projection =
        Projection::new(&doc, &[Chunk::Copy(bytes(0, 3)), Chunk::Copy(bytes(3, 6))]).unwrap();
    assert_eq!(projection.mappings().len(), 1);
    assert_eq!(
        projection.to_source(bytes(1, 5)).unwrap(),
        Some(bytes(1, 5))
    );
    assert_eq!(
        projection.to_source(bytes(3, 3)).unwrap(),
        Some(bytes(3, 3))
    );
}

#[test]
fn reordered_and_repeated_copies_do_not_map_across_boundaries() {
    let doc = document("abcdef");
    let projection = Projection::new(
        &doc,
        &[
            Chunk::Copy(bytes(3, 6)),
            Chunk::Copy(bytes(0, 3)),
            Chunk::Copy(bytes(0, 3)),
        ],
    )
    .unwrap();
    assert_eq!(projection.text(), "defabcabc");
    assert_eq!(projection.to_source(bytes(2, 4)).unwrap(), None);
    assert_eq!(projection.to_source(bytes(3, 3)).unwrap(), None);
    assert_eq!(projection.to_source(bytes(6, 6)).unwrap(), None);
    assert_eq!(
        projection.to_source(bytes(6, 9)).unwrap(),
        Some(bytes(0, 3))
    );
}

#[test]
fn unambiguous_start_end_and_interior_points_map() {
    let doc = document("abc");
    let projection = Projection::new(&doc, &[Chunk::Copy(bytes(0, 3))]).unwrap();
    for point in 0..=3 {
        assert_eq!(
            projection.to_source(bytes(point, point)).unwrap(),
            Some(bytes(point, point))
        );
    }
}

#[test]
fn invalid_copy_and_generated_boundaries_are_errors() {
    let doc = document("😀");
    for range in [bytes(1, 4), bytes(0, 5), bytes(4, 0)] {
        assert!(Projection::new(&doc, &[Chunk::Copy(range)]).is_err());
    }
    let projection = Projection::new(&doc, &[Chunk::Copy(bytes(0, 4))]).unwrap();
    for range in [bytes(0, 2), bytes(5, 5), bytes(3, 0)] {
        assert!(projection.to_source(range).is_err());
    }
}

#[test]
fn empty_copy_does_not_invent_a_mapping() {
    let doc = document("abc");
    let projection = Projection::new(&doc, &[Chunk::Copy(bytes(1, 1))]).unwrap();
    assert!(projection.mappings().is_empty());
    assert_eq!(projection.to_source(bytes(0, 0)).unwrap(), None);
}

#[test]
fn projection_remains_tied_to_original_snapshot_after_edit() {
    let doc = document("abc");
    let projection = Projection::new(&doc, &[Chunk::Copy(bytes(0, 3))]).unwrap();
    let next = doc
        .edited(
            2,
            &[Edit {
                range: None,
                text: "changed".into(),
            }],
        )
        .unwrap();
    assert_eq!(projection.source().snapshot().version, 1);
    assert_eq!(projection.text(), "abc");
    assert_eq!(next.snapshot().version, 2);
}

#[test]
fn projection_expansion_is_bounded() {
    let doc = document(&"x".repeat(1024 * 1024));
    let chunks = vec![Chunk::Copy(bytes(0, doc.text().len())); 17];
    assert_eq!(
        Projection::new(&doc, &chunks).unwrap_err().code,
        "size_limit"
    );
}

#[test]
fn malformed_protocol_requests_are_structured_errors() {
    for input in [
        b"not JSON".as_slice(),
        b"\xff",
        br#"{"op":"check"}"#,
        br#"{"op":"capabilities","extra":true}"#,
    ] {
        let response: serde_json::Value =
            serde_json::from_slice(&ts_svelte::protocol::dispatch_json(input)).unwrap();
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "invalid_request");
    }
}

#[test]
fn capabilities_do_not_claim_svelte_support() {
    let response: serde_json::Value = serde_json::from_slice(&ts_svelte::protocol::dispatch_json(
        br#"{"op":"capabilities"}"#,
    ))
    .unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["svelteParsing"], false);
    assert_eq!(response["result"]["typeChecking"], false);
    assert_eq!(response["result"]["compilation"], false);
}

#[test]
fn protocol_budgets_bound_operation_count_and_repeated_scans() {
    let inputs = [
        serde_json::json!({"op":"locate", "document":document("").snapshot(), "positions":[], "offsets":vec![0;4097]}),
        serde_json::json!({"op":"locate", "document":document(&"x".repeat(1024*1024)).snapshot(), "positions":[], "offsets":vec![1024*1024;129]}),
        serde_json::json!({"op":"edit", "document":document("").snapshot(), "version":2,
            "edits":std::iter::once(Edit { range:None, text:"x".repeat(1024*1024) }).chain(std::iter::repeat_n(Edit {range:None,text:String::new()},129)).collect::<Vec<_>>()}),
    ];
    for input in inputs {
        let response: serde_json::Value = serde_json::from_slice(
            &ts_svelte::protocol::dispatch_json(&serde_json::to_vec(&input).unwrap()),
        )
        .unwrap();
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "size_limit");
    }
}

use super::*;
use crate::{
    imports::ImportOptions,
    snapshot::{ResolvedImport, StylesheetSnapshot},
    stylesheet::{StylesheetOptions, prepare_stylesheet},
};
fn fixture(css: &str) -> (ProjectSnapshot, StylesheetBuild) {
    let snapshot = ProjectSnapshot {
        tailwind_version: crate::TAILWIND_VERSION.into(),
        entry: "app.css".into(),
        stylesheets: vec![StylesheetSnapshot {
            id: "app.css".into(),
            version: 7,
            css: css.into(),
        }],
        sources: vec![],
        imports: vec![],
        plugins: vec![],
    };
    let build = build(&snapshot, &[]);
    (snapshot, build)
}
fn build(snapshot: &ProjectSnapshot, candidates: &[&str]) -> StylesheetBuild {
    let options = StylesheetOptions {
        imports: ImportOptions {
            bases: snapshot
                .stylesheets
                .iter()
                .map(|sheet| (sheet.id.clone(), "/project".into()))
                .collect(),
            limits: Default::default(),
        },
        property_fallbacks: false,
        license_comment: false,
    };
    prepare_stylesheet(snapshot, &options)
        .unwrap()
        .build(&candidates.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap()
}
// Independent v3 decoder, used to inspect the serialized public artifact rather
// than merely comparing the generator's internal Mapping structs.
fn decode(map: &Value) -> Vec<(u32, u32, Option<(usize, u32, u32)>)> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut source = 0i64;
    let mut original_line = 0i64;
    let mut original_column = 0i64;
    let mut result = Vec::new();
    for (line, segments) in map["mappings"].as_str().unwrap().split(';').enumerate() {
        let mut generated_column = 0i64;
        for segment in segments.split(',').filter(|s| !s.is_empty()) {
            let mut numbers = Vec::new();
            let mut value = 0u64;
            let mut shift = 0;
            for byte in segment.bytes() {
                let digit = alphabet.iter().position(|b| *b == byte).unwrap() as u64;
                value |= (digit & 31) << shift;
                if digit & 32 == 0 {
                    numbers.push(if value & 1 == 0 {
                        (value >> 1) as i64
                    } else {
                        -((value >> 1) as i64)
                    });
                    value = 0;
                    shift = 0;
                } else {
                    shift += 5;
                }
            }
            assert_eq!(shift, 0);
            assert!(numbers.len() == 1 || numbers.len() == 4);
            generated_column += numbers[0];
            assert!(generated_column >= 0);
            let original = if numbers.len() == 4 {
                source += numbers[1];
                original_line += numbers[2];
                original_column += numbers[3];
                assert!(source >= 0 && original_line >= 0 && original_column >= 0);
                Some((
                    source as usize,
                    original_line as u32,
                    original_column as u32,
                ))
            } else {
                None
            };
            result.push((line as u32, generated_column as u32, original));
        }
    }
    result
}
fn lookup(map: &Value, line: u32, column: u32) -> Option<(String, u32, u32)> {
    decode(map)
        .into_iter()
        .filter(|(l, c, _)| *l == line && *c <= column)
        .last()
        .and_then(|(_, _, original)| original)
        .map(|(source, line, column)| {
            (
                map["sources"][source].as_str().unwrap().into(),
                line,
                column,
            )
        })
}
#[test]
fn vlq_output_has_standard_signed_deltas_and_generated_only_segments() {
    let mappings = vec![
        Mapping {
            generated: Position { line: 0, column: 0 },
            original: Some(Original {
                source: 0,
                position: Position { line: 0, column: 0 },
            }),
        },
        Mapping {
            generated: Position { line: 0, column: 1 },
            original: Some(Original {
                source: 0,
                position: Position { line: 0, column: 1 },
            }),
        },
        Mapping {
            generated: Position { line: 1, column: 0 },
            original: None,
        },
        Mapping {
            generated: Position { line: 1, column: 2 },
            original: Some(Original {
                source: 0,
                position: Position { line: 0, column: 0 },
            }),
        },
    ];
    assert_eq!(encode(&mappings), "AAAA,CAAC;A,EAAD");
}
#[test]
fn copied_non_bmp_crlf_locations_are_utf16_and_sources_content_is_original() {
    let (snapshot, build) = fixture(".😀 { color: red; }\r\n.a { content: '💡'; }");
    let map = generate(&build, &snapshot).unwrap();
    assert_eq!(map["version"], 3);
    assert_eq!(map["file"], "app.css");
    assert_eq!(map["sourcesContent"][0], snapshot.stylesheets[0].css);
    // First declaration originally begins after the two-unit emoji selector.
    assert_eq!(lookup(&map, 1, 2), Some(("app.css".into(), 0, 6)));
    // Generated end points retain UTF-16 widths for a non-BMP string value.
    let declaration = build.ast[1].children().unwrap()[0].dst.as_ref().unwrap();
    let dst = LineTable::new(&build.css)
        .position(declaration.end)
        .unwrap();
    assert_eq!(
        lookup(&map, dst.line, dst.column),
        Some(("app.css".into(), 1, 18))
    );
    assert_eq!(lookup(&map, 3, 0), Some(("app.css".into(), 1, 0)));
}
#[test]
fn line_table_checkpoints_keep_utf16_and_crlf_semantics() {
    let text = format!("{}😀\r\nnext\rstill", "é".repeat(300));
    let table = LineTable::new(&text);
    assert_eq!(
        table.position(600).unwrap(),
        Position {
            line: 0,
            column: 300
        }
    );
    assert_eq!(
        table.position(604).unwrap(),
        Position {
            line: 0,
            column: 302
        }
    );
    assert_eq!(
        table.position(606).unwrap(),
        Position { line: 1, column: 0 }
    );
    assert_eq!(
        table.position(text.len()).unwrap(),
        Position {
            line: 1,
            column: 10
        }
    );
    assert!(table.position(601).is_err());
}
#[test]
fn transformed_node_keeps_provenance_without_fake_character_mappings() {
    let (snapshot, mut build) = fixture(".a { color: red; }");
    let node = &mut build.ast[0].children_mut().unwrap()[0];
    if let NodeKind::Declaration { value, .. } = &mut node.kind {
        *value = Some("color-mix(in srgb, red 50%, transparent)".into());
    }
    build.css = to_css(&mut build.ast, "app.css").unwrap();
    let map = generate(&build, &snapshot).unwrap();
    assert_eq!(map["x_ts_rust_mappingMode"], "provenance-anchors");
    assert_eq!(lookup(&map, 1, 2), Some(("app.css".into(), 0, 5)));
    let mappings = decode(&map);
    let declaration_anchors = mappings
        .iter()
        .filter(|(line, _, original)| *line == 1 && original.is_some())
        .count();
    assert_eq!(
        declaration_anchors, 2,
        "Only node start/end are asserted, not transformed token characters"
    );
}
#[test]
fn closing_braces_indentation_and_generated_helpers_are_unmapped() {
    let (snapshot, mut build) = fixture(".a { color: red; }");
    build.ast.push(AstNode::new(NodeKind::Rule {
        selector: ".compiler-helper".into(),
        nodes: vec![AstNode::declaration("display", "block")],
    }));
    build.css = to_css(&mut build.ast, "app.css").unwrap();
    let map = generate(&build, &snapshot).unwrap();
    assert_eq!(lookup(&map, 1, 0), None);
    assert_eq!(lookup(&map, 1, 1), None);
    assert_eq!(lookup(&map, 2, 0), None);
    assert_eq!(lookup(&map, 2, 1), None);
    assert_eq!(lookup(&map, 3, 0), None);
    assert_eq!(lookup(&map, 4, 2), None);
    assert_eq!(lookup(&map, 5, 0), None);
    // Upstream's end anchor maps the punctuation position itself. The next
    // column is explicitly unmapped and cannot inherit that source position.
    let end = build.ast[0].children().unwrap()[0]
        .dst
        .as_ref()
        .unwrap()
        .end;
    let end = LineTable::new(&build.css).position(end).unwrap();
    assert!(lookup(&map, end.line, end.column).is_some());
    assert_eq!(lookup(&map, end.line, end.column + 1), None);
}
#[test]
fn imported_theme_and_custom_utility_maps_use_actual_css_sources() {
    let (mut snapshot, _) = fixture("");
    snapshot.stylesheets[0].css =
        "@import './theme.css'; @import './utils.css'; @tailwind utilities;".into();
    snapshot.stylesheets.extend([
        StylesheetSnapshot {
            id: "mem://theme/😀.css".into(),
            version: 2,
            css: "@theme { --color-brand: red; }".into(),
        },
        StylesheetSnapshot {
            id: "mem://utils.css".into(),
            version: 3,
            css: "@utility card { color: blue; }".into(),
        },
    ]);
    snapshot.imports = vec![
        ResolvedImport {
            from: "app.css".into(),
            specifier: "./theme.css".into(),
            target: "mem://theme/😀.css".into(),
        },
        ResolvedImport {
            from: "app.css".into(),
            specifier: "./utils.css".into(),
            target: "mem://utils.css".into(),
        },
    ];
    let build = build(&snapshot, &["bg-brand", "card"]);
    let map = generate(&build, &snapshot).unwrap();
    let sources = map["sources"].as_array().unwrap();
    assert!(sources.iter().any(|id| id == "mem://theme/😀.css"));
    assert!(sources.iter().any(|id| id == "mem://utils.css"));
    for (index, id) in sources.iter().enumerate() {
        let sheet = snapshot
            .stylesheets
            .iter()
            .find(|sheet| Some(sheet.id.as_str()) == id.as_str())
            .unwrap();
        assert_eq!(map["sourcesContent"][index], sheet.css);
    }
    let nodes = tree(&build.ast).unwrap();
    for property in ["--color-brand", "color"] {
        let node = nodes
            .iter()
            .find(|node| matches!(&node.kind,NodeKind::Declaration{property:p,..} if p==property))
            .unwrap();
        let src = node.src.as_ref().unwrap();
        let dst = node.dst.as_ref().unwrap();
        let position = LineTable::new(&build.css).position(dst.start).unwrap();
        assert_eq!(
            lookup(&map, position.line, position.column).unwrap().0,
            src.file
        );
    }
}
#[test]
fn empty_ast_produces_valid_empty_source_list_and_no_original_mappings() {
    let (snapshot, build) = fixture("");
    let map = generate(&build, &snapshot).unwrap();
    assert_eq!(map["sources"], json!([]));
    assert_eq!(map["sourcesContent"], json!([]));
    assert_eq!(map["names"], json!([]));
    assert!(
        decode(&map)
            .iter()
            .all(|(_, _, original)| original.is_none())
    );
}
#[test]
fn multiline_originals_preserve_upstream_duplicate_start_anchors() {
    let (snapshot, build) = fixture("/*! first\n second */");
    let map = generate(&build, &snapshot).unwrap();
    let positions = decode(&map)
        .into_iter()
        .filter(|(line, column, _)| *line == 0 && *column == 0)
        .filter_map(|(_, _, p)| p)
        .collect::<Vec<_>>();
    assert_eq!(positions, vec![(0, 0, 0), (0, 1, 0)]);
}
#[test]
fn stale_receipts_fail_on_version_and_same_length_content_changes() {
    let (mut snapshot, build) = fixture(".a { color: red; }");
    snapshot.stylesheets[0].version += 1;
    assert!(generate(&build, &snapshot).unwrap_err().contains("Stale"));
    snapshot.stylesheets[0].version -= 1;
    snapshot.stylesheets[0].css = snapshot.stylesheets[0].css.replace("red", "tan");
    assert!(generate(&build, &snapshot).unwrap_err().contains("Stale"));
}
#[test]
fn missing_source_receipts_and_referenced_id_fail() {
    let (snapshot, mut build) = fixture(".a { color: red; }");
    build.source_receipts.clear();
    assert!(generate(&build, &snapshot).is_err());
    let (snapshot, mut build) = fixture(".a { color: red; }");
    build.ast[0].src.as_mut().unwrap().file = "absent.css".into();
    assert!(
        generate(&build, &snapshot)
            .unwrap_err()
            .contains("Missing original")
    );
}
#[test]
fn stale_generated_text_ranges_and_invalid_utf8_boundaries_fail() {
    let (snapshot, mut build) = fixture(".😀 { color: red; }");
    build.css.push(' ');
    assert!(generate(&build, &snapshot).unwrap_err().contains("printer"));
    let (snapshot, mut build) = fixture(".😀 { color: red; }");
    build.ast[0].dst.as_mut().unwrap().end += 1;
    assert!(generate(&build, &snapshot).unwrap_err().contains("ranges"));
    let (snapshot, mut build) = fixture(".😀 { color: red; }");
    build.ast[0].src.as_mut().unwrap().start = 2;
    assert!(
        generate(&build, &snapshot).unwrap_err().contains("UTF")
            || generate(&build, &snapshot)
                .unwrap_err()
                .contains("byte range")
    );
    build.ast[0].src.as_mut().unwrap().start = usize::MAX;
    assert!(generate(&build, &snapshot).is_err());
}
#[test]
fn deterministic_sources_and_map_ignore_snapshot_input_order() {
    let (mut snapshot, _) = fixture(".a { color: red; }");
    snapshot.stylesheets.push(StylesheetSnapshot {
        id: "unused.css".into(),
        version: 0,
        css: "".into(),
    });
    let build = build(&snapshot, &[]);
    let first = generate(&build, &snapshot).unwrap();
    snapshot.stylesheets.reverse();
    assert_eq!(generate(&build, &snapshot).unwrap(), first);
    assert_eq!(first["sources"], json!(["app.css"]));
}

//! Retained SvelteKit feature-marker collection after bundler tree shaking.
//!
//! Derived from MIT-licensed SvelteKit renderChunk in Kit 2 exports/vite/index.js
//! and Kit 3 exports/vite/build/index.js; copyright Svelte contributors. Both
//! registered families collect __SVELTEKIT_TRACK__('label') occurrences and
//! remove those calls. This implementation uses the emitted AST rather than
//! matching strings/comments with a regular expression. Keys are actual emitted
//! filenames, not chunk.name plus an assumed .js suffix.
//!
//! A result proves that a feature marker survived in a supplied emitted chunk,
//! not that an application executed that feature. Call only after tree shaking.
//! Empty arrays explicitly mean no retained marker in that supplied chunk; no
//! claims are made about omitted chunks. Labels retain source order/duplicates.
//! Direct, unbound, non-optional one-string-argument calls become `(void 0)`;
//! this is valid in expression and statement contexts after minification.
//! Shadowed markers and unsupported reference/call forms fail rather than
//! rewriting user functions. Marker-looking comments, strings and property
//! names remain unchanged. No executable code/configuration is evaluated.
//!
//! Source-map/URL edits share build_finalize's Preserve/Drop/Reject machinery.
//! Preserve moves only actual anchors and leaves generated replacements unmapped.
//! Original call spans are emitted-chunk UTF-8 offsets, not guessed source spans.

use std::collections::{BTreeMap, BTreeSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, BindingIdentifier, CallExpression, Expression, IdentifierReference};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::build_finalize::{JavaScriptArtifact, SourceMapPolicy, rewrite_generated_artifact};
use crate::sync_versions::KitSyncVersion;

const MARKER: &str = "__SVELTEKIT_TRACK__";
const MAX_CHUNK: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 128 * 1024 * 1024;
const MAX_MARKERS: usize = 100_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackingInput {
    pub kit_version: String,
    /// Explicit host assertion that these are retained emitted chunks.
    pub after_tree_shaking: bool,
    pub artifacts: BTreeMap<String, JavaScriptArtifact>,
    pub source_maps: SourceMapPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedOccurrence {
    /// Parsed JavaScript string value, including escaped/Unicode labels.
    pub label: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackingOutput {
    pub artifacts: BTreeMap<String, JavaScriptArtifact>,
    pub tracked_features: BTreeMap<String, Vec<String>>,
    pub occurrences: BTreeMap<String, Vec<TrackedOccurrence>>,
    pub updated_map_files: BTreeMap<String, Value>,
    pub removed_map_files: BTreeSet<String>,
    pub removed_mapping_urls: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
struct MarkerScan {
    references: Vec<Span>,
    callees: BTreeSet<(u32, u32)>,
    bindings: Vec<Span>,
    calls: Vec<TrackedOccurrence>,
    errors: Vec<String>,
}
impl<'a> Visit<'a> for MarkerScan {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if identifier.name == MARKER {
            self.references.push(identifier.span);
        }
    }
    fn visit_binding_identifier(&mut self, identifier: &BindingIdentifier<'a>) {
        if identifier.name == MARKER {
            self.bindings.push(identifier.span);
        }
    }
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(identifier) = &call.callee {
            if identifier.name == MARKER {
                self.callees
                    .insert((identifier.span.start, identifier.span.end));
                let label = match call.arguments.as_slice() {
                    [Argument::StringLiteral(value)]
                        if !call.optional && call.type_arguments.is_none() =>
                    {
                        Some(value.value.to_string())
                    }
                    _ => None,
                };
                match label {
                    Some(label) if !label.is_empty() => self.calls.push(TrackedOccurrence { label, start: call.span.start as usize, end: call.span.end as usize }),
                    _ => self.errors.push(format!("Unsupported tracking call at byte {}: expected a non-optional call with one nonempty string literal", call.span.start)),
                }
            }
        }
        walk::walk_call_expression(self, call);
    }
}

fn markers(code: &str) -> Result<Vec<TrackedOccurrence>, String> {
    if code.len() > MAX_CHUNK {
        return Err("Tracking chunk exceeds 16 MiB".into());
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("Invalid emitted JavaScript: {error}"));
    }
    let mut scan = MarkerScan::default();
    scan.visit_program(&parsed.program);
    if let Some(binding) = scan.bindings.first() {
        return Err(format!(
            "Tracking marker is locally bound or shadowed at byte {}; refusing to rewrite application code",
            binding.start
        ));
    }
    if let Some(error) = scan.errors.first() {
        return Err(error.clone());
    }
    if scan.references.len() > MAX_MARKERS {
        return Err("Too many emitted tracking markers".into());
    }
    for reference in scan.references {
        if code.get(reference.start as usize..reference.end as usize) != Some(MARKER) {
            return Err("Escaped tracking identifiers are unsupported".into());
        }
        if !scan.callees.contains(&(reference.start, reference.end)) {
            return Err(format!(
                "Unsupported tracking marker reference at byte {}: only direct calls may be removed",
                reference.start
            ));
        }
    }
    scan.calls.sort_by_key(|call| call.start);
    for pair in scan.calls.windows(2) {
        if pair[0].end > pair[1].start {
            return Err("Overlapping tracking calls are unsupported".into());
        }
    }
    Ok(scan.calls)
}

pub fn collect_emitted_features(input: &TrackingInput) -> Result<TrackingOutput, String> {
    // Both registered Kit families have the same marker call/label contract.
    KitSyncVersion::parse(&input.kit_version)?;
    if !input.after_tree_shaking {
        return Err("Feature tracking requires actual emitted chunks after tree shaking".into());
    }
    if input.artifacts.len() > 100_000 {
        return Err("Too many emitted tracking chunks".into());
    }
    let mut output = TrackingOutput {
        artifacts: BTreeMap::new(),
        tracked_features: BTreeMap::new(),
        occurrences: BTreeMap::new(),
        updated_map_files: BTreeMap::new(),
        removed_map_files: BTreeSet::new(),
        removed_mapping_urls: BTreeMap::new(),
    };
    let mut total = 0usize;
    let mut map_files = BTreeSet::new();
    for (name, artifact) in &input.artifacts {
        total = total
            .checked_add(artifact.code.len())
            .ok_or("Tracking source budget overflow")?;
        if total > MAX_TOTAL {
            return Err("Emitted tracking chunks exceed 128 MiB".into());
        }
        if let Some(file) = &artifact.map_file {
            if !map_files.insert(file) || input.artifacts.contains_key(file) {
                return Err("Tracking source-map artifact has ambiguous ownership".into());
            }
        }
        let calls = markers(&artifact.code).map_err(|error| format!("{name}: {error}"))?;
        let edits = calls
            .iter()
            .map(|call| (call.start, call.end, "(void 0)".into()))
            .collect();
        let rewritten = rewrite_generated_artifact(name, artifact, edits, input.source_maps)?;
        output.artifacts.extend(rewritten.artifacts);
        output.updated_map_files.extend(rewritten.updated_map_files);
        output.removed_map_files.extend(rewritten.removed_map_files);
        output
            .removed_mapping_urls
            .extend(rewritten.removed_mapping_urls);
        output.tracked_features.insert(
            name.clone(),
            calls.iter().map(|call| call.label.clone()).collect(),
        );
        output.occurrences.insert(name.clone(), calls);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn artifact(code: &str) -> JavaScriptArtifact {
        JavaScriptArtifact {
            code: code.into(),
            map: None,
            map_file: None,
            pending: vec![],
            receipt_sha256: None,
        }
    }
    fn input(version: &str, code: &str) -> TrackingInput {
        TrackingInput {
            kit_version: version.into(),
            after_tree_shaking: true,
            artifacts: BTreeMap::from([("chunks/server-Ab12.mjs".into(), artifact(code))]),
            source_maps: SourceMapPolicy::Preserve,
        }
    }
    #[test]
    fn actual_read_marker_is_collected_by_emitted_filename_in_all_registered_versions() {
        let code =
            "export function read(asset){__SVELTEKIT_TRACK__('$app/server:read');return asset;}";
        for version in [
            "2.67.0",
            "2.68.0",
            "2.69.2",
            "2.69.3",
            "2.70.3",
            "3.0.0-next.32",
            "3.0.1",
        ] {
            let output = collect_emitted_features(&input(version, code)).unwrap();
            assert_eq!(
                output.tracked_features["chunks/server-Ab12.mjs"],
                ["$app/server:read"]
            );
            assert_eq!(
                output.artifacts["chunks/server-Ab12.mjs"].code,
                "export function read(asset){(void 0);return asset;}"
            );
            assert_eq!(
                output.occurrences["chunks/server-Ab12.mjs"][0].start,
                code.find(MARKER).unwrap()
            );
        }
    }
    #[test]
    fn minified_expression_calls_keep_order_duplicate_labels_and_valid_syntax() {
        let code = "export const x=(__SVELTEKIT_TRACK__(\"a\"),1),y=__SVELTEKIT_TRACK__('a')??2;__SVELTEKIT_TRACK__('b\\u003ac');";
        let output = collect_emitted_features(&input("3.0.1", code)).unwrap();
        assert_eq!(
            output.tracked_features["chunks/server-Ab12.mjs"],
            ["a", "a", "b:c"]
        );
        assert_eq!(
            output.artifacts["chunks/server-Ab12.mjs"].code,
            "export const x=((void 0),1),y=(void 0)??2;(void 0);"
        );
    }
    #[test]
    fn marker_strings_comments_and_property_names_are_untouched() {
        let code = "// __SVELTEKIT_TRACK__('fake')\nconst s=\"__SVELTEKIT_TRACK__('fake')\"; const o={__SVELTEKIT_TRACK__:1};";
        let request = input("2.70.3", code);
        let output = collect_emitted_features(&request).unwrap();
        assert!(output.tracked_features["chunks/server-Ab12.mjs"].is_empty());
        assert_eq!(output.artifacts, request.artifacts);
    }
    #[test]
    fn shadowed_and_unsupported_markers_fail_instead_of_removing_user_code() {
        for code in [
            "function f(__SVELTEKIT_TRACK__){__SVELTEKIT_TRACK__('x')}",
            "const __SVELTEKIT_TRACK__=()=>1;__SVELTEKIT_TRACK__('x');",
            "import {foo as __SVELTEKIT_TRACK__} from 'pkg';__SVELTEKIT_TRACK__('x');",
            "__SVELTEKIT_TRACK__(name);",
            "__SVELTEKIT_TRACK__('x', sideEffect());",
            "__SVELTEKIT_TRACK__?.('x');",
            "new __SVELTEKIT_TRACK__('x');",
            "const marker=__SVELTEKIT_TRACK__;",
            "__SVELTEKIT_TRACK__=fn;",
            "__SVELTEKIT_TRACK__('');",
            "__SVELTEKIT_TRACK__(`x`);",
            "__SVELTEKIT_\\u0054RACK__('x');",
        ] {
            assert!(
                collect_emitted_features(&input("3.0.1", code)).is_err(),
                "{code}"
            );
        }
    }
    #[test]
    fn utf16_source_maps_retain_metadata_and_shift_real_trailing_anchor() {
        let code = "const s='🙂';__SVELTEKIT_TRACK__('$app/server:read');tail();\n//# sourceMappingURL=server-Ab12.mjs.map";
        let mut request = input("3.0.1", code);
        // Generated columns: 0 and the actual UTF-16 column of tail(). The
        // source-map VLQ column delta is generated independently in the test.
        let column = code[..code.find("tail()").unwrap()].encode_utf16().count();
        fn vlq(mut value: usize) -> String {
            value <<= 1;
            let base = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut result = String::new();
            loop {
                let digit = value & 31;
                value >>= 5;
                result.push(base[digit | if value > 0 { 32 } else { 0 }] as char);
                if value == 0 {
                    break;
                }
            }
            result
        }
        // A five-field segment is generated column + source + line + column +
        // name. Tail maps to original column 8, source/name index zero.
        let map = serde_json::json!({"version":3,"sources":["runtime.ts"],"sourceRoot":"../src/","sourcesContent":["read(); tail();"],"names":["tail"],"mappings":format!("AAAA,{}AAQA",vlq(column))});
        let entry = request.artifacts.get_mut("chunks/server-Ab12.mjs").unwrap();
        entry.map = Some(map.clone());
        entry.map_file = Some("chunks/server-Ab12.mjs.map".into());
        let output = collect_emitted_features(&request).unwrap();
        let result = &output.artifacts["chunks/server-Ab12.mjs"];
        assert_eq!(
            result.code,
            "const s='🙂';(void 0);tail();\n//# sourceMappingURL=server-Ab12.mjs.map"
        );
        assert_eq!(
            result.map.as_ref().unwrap()["sourcesContent"],
            map["sourcesContent"]
        );
        assert_eq!(
            result.map.as_ref().unwrap()["sourceRoot"],
            map["sourceRoot"]
        );
        // New anchors: mapped 0, unmapped marker start at UTF16 column 13,
        // unmapped marker end at 21, mapped tail at 22.
        assert_eq!(result.map.as_ref().unwrap()["mappings"], "AAAA,a,Q,CAAQA");
        assert_eq!(
            output.updated_map_files["chunks/server-Ab12.mjs.map"],
            result.map.clone().unwrap()
        );
    }
    #[test]
    fn explicit_phase_and_manifest_receipt_ownership_are_required() {
        let mut request = input("3.0.1", "__SVELTEKIT_TRACK__('x');");
        request.after_tree_shaking = false;
        assert!(
            collect_emitted_features(&request)
                .unwrap_err()
                .contains("after tree shaking")
        );
        request.after_tree_shaking = true;
        request
            .artifacts
            .get_mut("chunks/server-Ab12.mjs")
            .unwrap()
            .receipt_sha256 = Some("stale".into());
        assert!(
            collect_emitted_features(&request)
                .unwrap_err()
                .contains("before manifest receipt")
        );
    }
}

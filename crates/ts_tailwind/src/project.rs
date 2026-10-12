//! Compose native source selection, Oxide extraction and stylesheet compilation.
//! Filesystem discovery is a host operation; every input used here is explicit.
use crate::{
    snapshot::{ProjectSnapshot, SourceSnapshot},
    source_selection::{SourceInventory, SourceSelection, select_sources},
    stylesheet::{PreparedStylesheet, StylesheetBuild, StylesheetOptions, prepare_stylesheet},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBuild {
    pub selection: SourceSelection,
    pub extraction: ts_tailwind_scan::ScanOutput,
    pub stylesheet: StylesheetBuild,
}
pub fn build_prepared(
    prepared: &PreparedStylesheet,
    sources: &[SourceSnapshot],
    inventory: &SourceInventory,
    scan_options: &ts_tailwind_scan::ScanOptions,
) -> Result<ProjectBuild, String> {
    let selection = select_sources(prepared.metadata(), sources, inventory)
        .map_err(|error| error.to_string())?;
    let selected: BTreeSet<_> = selection
        .selected
        .iter()
        .map(|source| source.id.as_str())
        .collect();
    let inputs = sources
        .iter()
        .filter(|source| selected.contains(source.id.as_str()))
        .map(|source| ts_tailwind_scan::SourceSnapshot {
            id: source.id.clone(),
            version: source.version,
            extension: source.extension.clone(),
            text: source.text.clone(),
        })
        .collect::<Vec<_>>();
    let extraction =
        ts_tailwind_scan::scan_sources(&inputs, scan_options).map_err(|error| error.to_string())?;
    let mut candidates = extraction.candidates.clone();
    candidates.extend(extraction.custom_properties.clone());
    let stylesheet = prepared
        .build(&candidates)
        .map_err(|error| error.to_string())?;
    Ok(ProjectBuild {
        selection,
        extraction,
        stylesheet,
    })
}
pub fn compile_project(
    snapshot: &ProjectSnapshot,
    options: &StylesheetOptions,
    inventory: &SourceInventory,
    scan_options: &ts_tailwind_scan::ScanOptions,
) -> Result<ProjectBuild, String> {
    let prepared = prepare_stylesheet(snapshot, options).map_err(|error| error.to_string())?;
    build_prepared(&prepared, &snapshot.sources, inventory, scan_options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn selected_svelte_inputs_generate_css_and_source_edits_remove_classes() {
        let snapshot: ProjectSnapshot = serde_json::from_value(json!({
            "tailwindVersion":"4.2.1","entry":"app.css",
            "stylesheets":[{"id":"app.css","version":1,"css":"@theme { --color-brand: red; } @tailwind utilities source(none); @source './src'; @source not './src/private.svelte';"}],
            "sources":[
                {"id":"component","version":1,"extension":"svelte","text":"<div class=\"bg-brand hover:flex\" class:underline={active}/>"},
                {"id":"excluded","version":1,"extension":"svelte","text":"<div class=\"block\"/>"}
            ]
        })).unwrap();
        let options: StylesheetOptions = serde_json::from_value(
            json!({"imports":{"bases":{"app.css":"/app"}},"licenseComment":false}),
        )
        .unwrap();
        let inventory: SourceInventory = serde_json::from_value(json!({"files":[{"id":"component","path":"/app/src/A.svelte","binary":false},{"id":"excluded","path":"/app/src/private.svelte","binary":false}],"automaticEligibleIds":["component","excluded"]})).unwrap();
        let prepared = prepare_stylesheet(&snapshot, &options).unwrap();
        let first = build_prepared(
            &prepared,
            &snapshot.sources,
            &inventory,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(first.selection.selected.len(), 1);
        assert!(
            first
                .stylesheet
                .css
                .contains("background-color: var(--color-brand)")
        );
        assert!(first.stylesheet.css.contains(".underline"));
        assert!(!first.stylesheet.css.contains(".block"));
        let mut sources = snapshot.sources.clone();
        sources[0].text = "<div class=\"grid\"/>".into();
        sources[0].version = 2;
        let second = build_prepared(&prepared, &sources, &inventory, &Default::default()).unwrap();
        assert!(second.stylesheet.css.contains(".grid"));
        assert!(!second.stylesheet.css.contains("--color-brand"));
        assert!(!second.stylesheet.css.contains(".underline"));
        assert_eq!(second.extraction.sources[0].version, 2);
    }
}

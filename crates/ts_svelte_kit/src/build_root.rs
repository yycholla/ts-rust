//! Pure Kit root component generation. Kit 2 source is derived from the pinned
//! `src/core/sync/write_root.js`; Kit 3 source is derived from the pinned
//! `src/runtime/components/root.svelte`.
//!
//! Kit 3 normally uses its installed runtime root and generates no root file.
//! Relocation is an explicit native-orchestration option, not upstream behavior.
//!
//! Derived SvelteKit portions are MIT licensed:
//! Copyright (c) 2020 these people (https://github.com/sveltejs/kit/graphs/contributors)
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
//! of the Software, and to permit persons to whom the Software is furnished to do
//! so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sync_versions::KitSyncVersion;

const LICENSE: &str = "Copyright (c) 2020 SvelteKit contributors (https://github.com/sveltejs/kit/graphs/contributors)\n\nPermission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the \"Software\"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:\n\nThe above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.\n\nTHE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedRootInput {
    pub kit_version: String,
    pub output: PathBuf,
    #[serde(default)]
    pub routes: Vec<ResolvedRootRoute>,
    #[serde(default = "default_svelte_major")]
    pub svelte_major: u32,
    #[serde(default)]
    pub handle_rendering_errors: bool,
    /// Forward this to the native Svelte compiler. Upstream root source
    /// generation itself does not branch on the async compiler option.
    #[serde(default)]
    pub async_compiler: bool,
    /// Required for Kit 3's installed runtime root and relocated Props import.
    #[serde(default)]
    pub runtime_directory: Option<PathBuf>,
    /// False preserves Kit 3's installed-runtime-root contract (no generated
    /// files). True emits a relocated root that the host must explicitly use.
    #[serde(default)]
    pub relocate_kit3_root: bool,
}

fn default_svelte_major() -> u32 {
    5
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedRootRoute {
    /// None is a non-page route. Entries are manifest node indices, with null
    /// slots preserved. The upstream `filter(Boolean)` does not count index 0.
    pub layouts: Option<Vec<Option<u32>>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RootEntry {
    Generated { component: PathBuf, entry: PathBuf },
    Installed { component: PathBuf, entry: PathBuf },
}

fn validate_path(path: &Path, name: &str) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(format!("{name} must be an absolute resolved path"));
    }
    if path.to_str().is_none_or(|path| path.contains('\0')) {
        return Err(format!("{name} must be UTF-8 without NUL bytes"));
    }
    Ok(())
}

fn validate(input: &ResolvedRootInput) -> Result<KitSyncVersion, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    validate_path(&input.output, "output")?;
    if !matches!(input.svelte_major, 4 | 5) {
        return Err("Root generation supports Svelte major 4 or 5 only".into());
    }
    if version.is_kit3() {
        if input.svelte_major != 5 {
            return Err("Registered Kit 3 roots require Svelte 5".into());
        }
        validate_path(
            input
                .runtime_directory
                .as_deref()
                .ok_or("Kit 3 root selection requires runtimeDirectory")?,
            "runtimeDirectory",
        )?;
    }
    if input.routes.len() > 100_000 {
        return Err("Root route count exceeds 100000".into());
    }
    Ok(version)
}

/// Identify the module that orchestration must load. Kit 2's Svelte 5 entry is
/// the compatibility class wrapper; Kit 3 is the runtime component itself.
/// # Errors
/// Rejects unknown profiles and unresolved module paths.
pub fn root_entry(input: &ResolvedRootInput) -> Result<RootEntry, String> {
    let version = validate(input)?;
    if version.is_kit3() && !input.relocate_kit3_root {
        let component = input
            .runtime_directory
            .as_ref()
            .expect("validated runtime directory")
            .join("components/root.svelte");
        Ok(RootEntry::Installed {
            entry: component.clone(),
            component,
        })
    } else {
        let component = input.output.join("root.svelte");
        let entry = if !version.is_kit3() && input.svelte_major >= 5 {
            input.output.join("root.js")
        } else {
            component.clone()
        };
        Ok(RootEntry::Generated { component, entry })
    }
}

fn indent(source: &str, levels: usize) -> String {
    let tabs = "\t".repeat(levels);
    source
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{tabs}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pyramid(max_depth: usize, svelte5: bool, boundaries: bool) -> String {
    if boundaries {
        return r#"{#snippet pyramid(depth)}
	{@const Pyramid = constructors[depth]}
	{#snippet failed(error)}
		{@const ErrorPage = errors[depth]}
		<ErrorPage {error} />
	{/snippet}
	<svelte:boundary failed={errors[depth] ? failed : undefined}>
		{#if constructors[depth + 1]}
			{@const d = data[depth]}
			<!-- svelte-ignore binding_property_non_reactive -->
			<Pyramid bind:this={components[depth]} data={d} {form} params={page.params}>
				{@render pyramid(depth + 1)}
			</Pyramid>
		{:else}
			{@const d = data[depth]}
			<!-- svelte-ignore binding_property_non_reactive -->
			<Pyramid bind:this={components[depth]} data={d} {form} params={page.params} {error} />
		{/if}
	</svelte:boundary>
{/snippet}

{@render pyramid(0)}"#
            .into();
    }
    let mut source = if svelte5 {
        format!(
            "<!-- svelte-ignore binding_property_non_reactive -->\n<Pyramid_{max_depth} bind:this={{components[{max_depth}]}} data={{data_{max_depth}}} {{form}} params={{page.params}} />"
        )
    } else {
        format!(
            "<svelte:component this={{constructors[{max_depth}]}} bind:this={{components[{max_depth}]}} data={{data_{max_depth}}} {{form}} params={{page.params}} />"
        )
    };
    for level in (0..max_depth).rev() {
        let branch = if svelte5 {
            format!(
                "{{@const Pyramid_{level} = constructors[{level}]}}\n<!-- svelte-ignore binding_property_non_reactive -->\n<Pyramid_{level} bind:this={{components[{level}]}} data={{data_{level}}} {{form}} params={{page.params}}>\n{}\n</Pyramid_{level}>",
                indent(&source, 1)
            )
        } else {
            format!(
                "<svelte:component this={{constructors[{level}]}} bind:this={{components[{level}]}} data={{data_{level}}} params={{page.params}}>\n{}\n</svelte:component>",
                indent(&source, 1)
            )
        };
        let leaf = if svelte5 {
            format!(
                "{{@const Pyramid_{level} = constructors[{level}]}}\n<!-- svelte-ignore binding_property_non_reactive -->\n<Pyramid_{level} bind:this={{components[{level}]}} data={{data_{level}}} {{form}} params={{page.params}} />"
            )
        } else {
            format!(
                "<svelte:component this={{constructors[{level}]}} bind:this={{components[{level}]}} data={{data_{level}}} {{form}} params={{page.params}} />"
            )
        };
        source = format!(
            "{{#if constructors[{}]}}\n{}\n\n{{:else}}\n{}\n\n{{/if}}",
            level + 1,
            indent(&branch, 1),
            indent(&leaf, 1)
        );
    }
    source
}

fn kit2_root(input: &ResolvedRootInput) -> Result<String, String> {
    let max_depth = input
        .routes
        .iter()
        .filter_map(|route| route.layouts.as_ref())
        .map(|layouts| {
            layouts
                .iter()
                .filter(|node| node.is_some_and(|index| index != 0))
                .count()
                + 1
        })
        .max()
        .unwrap_or(1)
        .max(1);
    if max_depth > 256 {
        return Err("Root layout depth exceeds 256".into());
    }
    let svelte5 = input.svelte_major >= 5;
    let boundaries = svelte5 && input.handle_rendering_errors;
    let levels = (0..=max_depth).collect::<Vec<_>>();
    let props = if svelte5 {
        let data = levels
            .iter()
            .map(|level| format!("data_{level} = null"))
            .collect::<Vec<_>>()
            .join(", ");
        let errors = if boundaries {
            "errors = [], error, "
        } else {
            ""
        };
        let derived = if boundaries {
            format!(
                "\nlet data = $derived({{{}}})",
                levels
                    .iter()
                    .map(|level| format!("'{level}': data_{level}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        };
        format!(
            "let {{ stores, page, constructors, components = [], form, {errors}{data} }} = $props();{derived}"
        )
    } else {
        format!(
            "export let stores;\nexport let page;\n\nexport let constructors;\nexport let components = [];\nexport let form;\n{}",
            levels
                .iter()
                .map(|level| format!("export let data_{level} = null;"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let page_update = if svelte5 {
        r#"if (browser) {
	$effect.pre(() => stores.page.set(page));
} else {
	// svelte-ignore state_referenced_locally
	stores.page.set(page);
}"#
    } else {
        "$: stores.page.set(page);"
    };
    let notify = if svelte5 {
        format!(
            "$effect(() => {{\n\tstores;page;constructors;components;form;{}{};\n\tstores.page.notify();\n}});",
            if boundaries { "errors;error;" } else { "" },
            levels
                .iter()
                .map(|level| format!("data_{level}"))
                .collect::<Vec<_>>()
                .join(";")
        )
    } else {
        "afterUpdate(stores.page.notify);".into()
    };
    let options = if svelte5 {
        "<svelte:options runes={true} />\n"
    } else {
        ""
    };
    let after_update = if svelte5 { "" } else { "afterUpdate, " };
    let state = if svelte5 { "$state(false)" } else { "false" };
    let title = if svelte5 { "$state(null)" } else { "null" };
    let constructor = if svelte5 {
        format!("\n\tconst Pyramid_{max_depth}=$derived(constructors[{max_depth}])\n")
    } else {
        String::new()
    };
    let pyramid = pyramid(max_depth, svelte5, boundaries);
    let props = indent(&props, 1);
    let page_update = indent(page_update, 1);
    let notify = indent(&notify, 1);
    Ok(format!(
        r#"<!-- Native generation derived from @sveltejs/kit write_root.js.
{LICENSE}
-->
{options}<script>
	import {{ setContext, {after_update}onMount, tick }} from 'svelte';
	import {{ browser }} from '$app/env';

	// stores
{props}

	if (!browser) {{
		// svelte-ignore state_referenced_locally
		setContext('__svelte__', stores);
	}}

{page_update}
{notify}

	let mounted = {state};
	let navigated = {state};
	let title = {title};

	onMount(() => {{
		const unsubscribe = stores.page.subscribe(() => {{
			if (mounted) {{
				navigated = true;
				tick().then(() => {{
					title = document.title || 'untitled page';
				}});
			}}
		}});

		mounted = true;
		return unsubscribe;
	}});
{constructor}</script>

{pyramid}

{{#if mounted}}
	<div id="svelte-announcer" aria-live="assertive" aria-atomic="true" style="position: absolute; left: 0; top: 0; clip: rect(0 0 0 0); clip-path: inset(50%); overflow: hidden; white-space: nowrap; width: 1px; height: 1px">
		{{#if navigated}}
			{{title}}
		{{/if}}
	</div>
{{/if}}
"#
    ))
}

fn relative(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut output = PathBuf::new();
    for _ in shared..from.len() {
        output.push("..");
    }
    for part in &to[shared..] {
        output.push(part.as_os_str());
    }
    let path = output.to_string_lossy().replace('\\', "/");
    if path.starts_with('.') {
        path
    } else {
        format!("./{path}")
    }
}

fn kit3_root(input: &ResolvedRootInput) -> Result<String, String> {
    let props = relative(
        &input.output,
        &input
            .runtime_directory
            .as_ref()
            .ok_or("Missing runtimeDirectory")?
            .join("props.svelte.js"),
    );
    let props = serde_json::to_string(&props).map_err(|error| error.to_string())?;
    Ok(format!(
        r#"<!-- Explicitly relocated native root; Kit 3 normally uses its installed runtime root.
{LICENSE}
-->
<script lang="ts">
	import {{ afterNavigate }} from '$app/navigation';
	import type {{ Props, RenderNode }} from {props};

	const {{ page, components, onerror, tree, form, error }}: Props = $props();

	let mounted = $state(false);
	let navigated = $state(false);
	let title = $state('');

	afterNavigate(() => {{
		if (mounted) {{
			navigated = true;
			title = document.title || 'untitled page';
		}} else {{
			mounted = true;
		}}
	}});
</script>

{{#snippet node(n: RenderNode, depth: number)}}
	{{const Component = $derived(n.component)}}
	{{const Error = $derived(n.error)}}
	{{const data = $derived(n.data)}}

	{{#snippet failed(error: unknown)}}
		<Error {{error}} />
	{{/snippet}}

	<svelte:boundary failed={{Error ? failed : undefined}} onerror={{Error ? onerror : undefined}}>
		{{#if n.child}}
			<!-- svelte-ignore binding_property_non_reactive -->
			<Component bind:this={{components[depth]}} {{data}} {{form}} params={{page.params}}>
				{{@render node(n.child, depth + 1)}}
			</Component>
		{{:else}}
			<!-- svelte-ignore binding_property_non_reactive -->
			<Component bind:this={{components[depth]}} {{data}} {{form}} params={{page.params}} {{error}} />
		{{/if}}
	</svelte:boundary>
{{/snippet}}

{{@render node(tree, 0)}}

{{#if mounted}}
	<div
		id="svelte-announcer"
		aria-live="assertive"
		aria-atomic="true"
		style="position: absolute; left: 0; top: 0; clip: rect(0 0 0 0); clip-path: inset(50%); overflow: hidden; white-space: nowrap; width: 1px; height: 1px"
	>
		{{#if navigated}}
			{{title}}
		{{/if}}
	</div>
{{/if}}
"#
    ))
}

/// Generate executable Svelte root source and (Kit 2/Svelte 5) its class wrapper.
/// Kit 3 returns no files by default: use `root_entry` to select its installed
/// component, or explicitly request relocation for a native build pipeline.
/// # Errors
/// Rejects unsupported profiles, unresolved paths and excessive route depth.
pub fn generate_root(input: &ResolvedRootInput) -> Result<BTreeMap<PathBuf, String>, String> {
    let version = validate(input)?;
    let mut files = BTreeMap::new();
    if version.is_kit3() {
        if input.relocate_kit3_root {
            files.insert(input.output.join("root.svelte"), kit3_root(input)?);
        }
    } else {
        files.insert(input.output.join("root.svelte"), kit2_root(input)?);
        if input.svelte_major >= 5 {
            files.insert(input.output.join("root.js"),format!("/*\n{LICENSE}\n*/\nimport {{ asClassComponent }} from 'svelte/legacy';\nimport Root from './root.svelte';\nexport default asClassComponent(Root);\n"));
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(version: &str) -> ResolvedRootInput {
        ResolvedRootInput {
            kit_version: version.into(),
            output: "/app/.svelte-kit/generated".into(),
            routes: vec![ResolvedRootRoute {
                layouts: Some(vec![Some(0), None, Some(2)]),
            }],
            svelte_major: 5,
            handle_rendering_errors: false,
            async_compiler: false,
            runtime_directory: Some("/app/node_modules/@sveltejs/kit/src/runtime".into()),
            relocate_kit3_root: false,
        }
    }
    #[test]
    fn kit2_depth_store_context_and_navigation_contract() {
        let input = input("2.69.2");
        let files = generate_root(&input).unwrap();
        let root = &files[&input.output.join("root.svelte")];
        assert!(root.contains("data_2 = null"));
        assert!(!root.contains("data_3"));
        assert!(root.contains("Pyramid_2=$derived(constructors[2])"));
        assert!(root.contains("{#if constructors[2]}"));
        assert!(root.contains("setContext('__svelte__', stores)"));
        assert!(root.contains("$effect.pre(() => stores.page.set(page))"));
        assert!(root.contains("stores;page;constructors;components;form;data_0;data_1;data_2;"));
        assert!(root.contains("tick().then(() =>"));
        assert!(root.contains("return unsubscribe"));
        assert!(root.contains("aria-live=\"assertive\""));
        assert!(files[&input.output.join("root.js")].contains("asClassComponent(Root)"));
        assert!(
            matches!(root_entry(&input).unwrap(),RootEntry::Generated{entry,..} if entry.ends_with("root.js"))
        );
    }
    #[test]
    fn rendering_boundaries_use_derived_data_and_error_components() {
        let mut input = input("2.70.3");
        input.handle_rendering_errors = true;
        let files = generate_root(&input).unwrap();
        let root = &files[&input.output.join("root.svelte")];
        assert!(root.contains("errors = [], error,"));
        assert!(root.contains("let data = $derived({'0': data_0, '1': data_1, '2': data_2})"));
        assert!(root.contains("{@const d = data[depth]}"));
        assert!(root.contains("failed={errors[depth] ? failed : undefined}"));
        assert!(root.contains("{@render pyramid(depth + 1)}"));
        assert!(root.contains("params={page.params} {error} />"));
    }
    #[test]
    fn svelte4_uses_legacy_reactivity_and_no_class_wrapper() {
        let mut input = input("2.67.0");
        input.svelte_major = 4;
        input.handle_rendering_errors = true;
        let files = generate_root(&input).unwrap();
        assert_eq!(files.len(), 1);
        let root = &files[&input.output.join("root.svelte")];
        assert!(root.contains("$: stores.page.set(page)"));
        assert!(root.contains("afterUpdate(stores.page.notify)"));
        assert!(root.contains("<svelte:component this={constructors[2]}"));
        assert!(!root.contains("$props()"));
        assert!(!root.contains("svelte:boundary"));
    }
    #[test]
    fn kit3_defaults_to_installed_runtime_and_relocation_is_explicit() {
        for version in ["3.0.1", "3.0.0-next.32"] {
            let mut input = input(version);
            assert!(generate_root(&input).unwrap().is_empty());
            assert!(
                matches!(root_entry(&input).unwrap(),RootEntry::Installed{entry,..} if entry.ends_with("runtime/components/root.svelte"))
            );
            input.relocate_kit3_root = true;
            let files = generate_root(&input).unwrap();
            assert_eq!(files.len(), 1);
            let root = &files[&input.output.join("root.svelte")];
            assert!(root.contains("import type { Props, RenderNode } from \"../../node_modules/@sveltejs/kit/src/runtime/props.svelte.js\""));
            assert!(root.contains("afterNavigate(() =>"));
            assert!(root.contains("{const Component = $derived(n.component)}"));
            assert!(root.contains("onerror={Error ? onerror : undefined}"));
            assert!(!root.contains("stores.page"));
        }
    }
    #[test]
    fn empty_routes_have_minimum_depth_and_unknown_profiles_fail() {
        let mut input = input("2.68.0");
        input.routes.clear();
        let files = generate_root(&input).unwrap();
        assert!(
            files[&input.output.join("root.svelte")]
                .contains("Pyramid_1=$derived(constructors[1])")
        );
        input.kit_version = "9.0.0".into();
        assert!(generate_root(&input).is_err());
        input.kit_version = "3.0.1".into();
        input.svelte_major = 4;
        assert!(generate_root(&input).is_err());
        input.svelte_major = 5;
        input.runtime_directory = None;
        assert!(generate_root(&input).is_err());
    }
}

//! Exact, offline compatibility selection shared by native and WASM hosts.
//!
//! A profile identifies the intended behavior, not full framework conformance.
//! Framework graph generation, builds and LSP remain separate capabilities.
use serde::{Deserialize, Serialize};

pub const DEFAULT_SVELTE: &str = "5.53.11";
pub const SVELTE_VERSIONS: &[&str] = &[
    "5.51.5", "5.52.0", "5.53.5", "5.53.11", "5.53.13", "5.54.1", "5.55.10", "5.56.10", "5.57.2",
];
pub const KIT_VERSIONS: &[&str] = &[
    "2.67.0",
    "2.68.0",
    "2.69.2",
    "2.69.3",
    "2.70.3",
    "3.0.1",
    "3.0.0-next.32",
];

fn default_svelte() -> String {
    DEFAULT_SVELTE.to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Versions {
    #[serde(default = "default_svelte")]
    pub svelte_version: String,
    #[serde(default)]
    pub kit_version: Option<String>,
    #[serde(default)]
    pub typescript_version: Option<String>,
    #[serde(default)]
    pub allow_prerelease: bool,
}

impl Default for Versions {
    fn default() -> Self {
        Self {
            svelte_version: default_svelte(),
            kit_version: None,
            typescript_version: None,
            allow_prerelease: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub svelte_version: String,
    pub kit_version: Option<String>,
    pub typescript_version: Option<String>,
    pub channel: &'static str,
    pub kit_config: Option<&'static str>,
    pub kit_library_alias: Option<&'static str>,
    pub kit_environment_module: Option<&'static str>,
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub component_compiler: &'static str,
    pub checking_projection: &'static str,
    pub project_check: &'static str,
    pub kit_graph: bool,
    pub kit_build: bool,
    pub lsp: bool,
}

impl Profile {
    #[must_use]
    pub fn svelte_minor(&self) -> u32 {
        self.svelte_version
            .split('.')
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }

    /// The facade currently targets the newer development-helper ABI.
    #[must_use]
    pub fn client_development(&self) -> bool {
        self.svelte_minor() >= 57
    }
}

/// Resolve exact installed versions. Dependency ranges and mutable tags are not
/// accepted; network resolution belongs to setup tooling, never the compiler.
///
/// # Errors
/// Returns an explanation for unregistered versions, unapproved prereleases or
/// incompatible framework/runtime/type-system combinations.
pub fn resolve(versions: &Versions) -> Result<Profile, String> {
    if !SVELTE_VERSIONS.contains(&versions.svelte_version.as_str()) {
        return Err(format!(
            "Unregistered Svelte version {}; exact supported profiles: {}",
            versions.svelte_version,
            SVELTE_VERSIONS.join(", ")
        ));
    }
    if let Some(ts) = &versions.typescript_version
        && !["5.9.3", "6.0.3"].contains(&ts.as_str())
    {
        return Err(format!(
            "Unregistered TypeScript toolchain target {ts}; use 5.9.3 or 6.0.3. The Rust checker is a fixed engine, not arbitrary TypeScript-version emulation."
        ));
    }
    let mut kit3 = false;
    let mut prerelease = false;
    if let Some(kit) = &versions.kit_version {
        if !KIT_VERSIONS.contains(&kit.as_str()) {
            return Err(format!(
                "Unregistered SvelteKit version {kit}; exact recognized profiles: {}",
                KIT_VERSIONS.join(", ")
            ));
        }
        prerelease = kit.contains('-');
        if prerelease && !versions.allow_prerelease {
            return Err("SvelteKit prerelease requires allowPrerelease: true".into());
        }
        kit3 = kit.starts_with("3.");
        if kit3 && versions.svelte_version != "5.57.2" {
            return Err(
                "This SvelteKit 3 profile requires Svelte 5.57.2 (upstream requires >=5.57.1)"
                    .into(),
            );
        }
        if kit3
            && versions
                .typescript_version
                .as_deref()
                .is_some_and(|v| v != "6.0.3")
        {
            return Err(
                "This SvelteKit 3 profile requires the TypeScript 6.0.3 toolchain target".into(),
            );
        }
    }
    let kit_config = versions.kit_version.as_ref().map(|_| {
        if kit3 {
            "vite-plugin"
        } else {
            "svelte-config-or-vite-plugin"
        }
    });
    Ok(Profile {
        id: format!(
            "v1:svelte={};kit={};ts={}",
            versions.svelte_version,
            versions.kit_version.as_deref().unwrap_or("none"),
            versions
                .typescript_version
                .as_deref()
                .unwrap_or("unspecified")
        ),
        svelte_version: versions.svelte_version.clone(),
        kit_version: versions.kit_version.clone(),
        typescript_version: versions.typescript_version.clone(),
        channel: if prerelease { "experimental" } else { "stable" },
        kit_config,
        kit_library_alias: versions
            .kit_version
            .as_ref()
            .map(|_| if kit3 { "#lib" } else { "$lib" }),
        kit_environment_module: versions
            .kit_version
            .as_ref()
            .map(|_| if kit3 { "$app/env" } else { "$app/environment" }),
        capabilities: Capabilities {
            component_compiler: "prototype",
            checking_projection: "prototype",
            project_check: "explicit-input-prototype",
            kit_graph: false,
            kit_build: false,
            lsp: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_retains_app_target() {
        let p = resolve(&Versions::default()).unwrap();
        assert_eq!(p.svelte_version, "5.53.11");
        assert!(!p.client_development());
        assert!(!p.capabilities.kit_build);
    }

    #[test]
    fn rejects_tags_ranges_unknowns_and_stale_prereleases() {
        for version in [
            "latest",
            "^5.53.11",
            "5.53.12",
            "5.50.0",
            "5.0.0-next.272",
            "5.57.2+custom",
        ] {
            assert!(
                resolve(&Versions {
                    svelte_version: version.into(),
                    ..Versions::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn framework_major_and_prerelease_are_explicit() {
        let mut v = Versions {
            svelte_version: "5.57.2".into(),
            kit_version: Some("3.0.0-next.32".into()),
            typescript_version: Some("6.0.3".into()),
            allow_prerelease: false,
        };
        assert!(resolve(&v).is_err());
        v.allow_prerelease = true;
        let p = resolve(&v).unwrap();
        assert_eq!(p.channel, "experimental");
        assert_eq!(p.kit_library_alias, Some("#lib"));
        v.typescript_version = Some("5.9.3".into());
        assert!(resolve(&v).is_err());
        v.typescript_version = Some("6.0.3".into());
        v.svelte_version = "5.53.11".into();
        assert!(resolve(&v).is_err());
    }

    #[test]
    fn identities_separate_workspace_versions() {
        let a = resolve(&Versions::default()).unwrap();
        let b = resolve(&Versions {
            svelte_version: "5.57.2".into(),
            ..Versions::default()
        })
        .unwrap();
        assert_ne!(a.id, b.id);
        for v in SVELTE_VERSIONS {
            assert!(
                resolve(&Versions {
                    svelte_version: (*v).into(),
                    ..Versions::default()
                })
                .is_ok()
            );
        }
    }
}

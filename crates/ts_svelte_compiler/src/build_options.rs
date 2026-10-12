//! Serializable build options. Reject unsupported semantics at the boundary.
use rsvelte_core::compiler::{CompileOptions, CssMode, Namespace};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildOptions {
    pub root_dir: Option<String>,
    pub sourcemap: Option<serde_json::Value>,
    pub css: Option<String>,
    pub preserve_comments: Option<bool>,
    pub preserve_whitespace: Option<bool>,
    pub disclose_version: Option<bool>,
    pub accessors: Option<bool>,
    pub immutable: Option<bool>,
    pub namespace: Option<String>,
    pub name: Option<String>,
    pub output_filename: Option<String>,
    pub css_output_filename: Option<String>,
    pub experimental: Option<Experimental>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experimental {
    pub r#async: bool,
}
impl BuildOptions {
    pub fn validate_module(&self) -> Result<(), String> {
        if self.sourcemap.is_some()
            || self.css.is_some()
            || self.preserve_comments.is_some()
            || self.preserve_whitespace.is_some()
            || self.disclose_version.is_some()
            || self.accessors.is_some()
            || self.immutable.is_some()
            || self.namespace.is_some()
            || self.name.is_some()
            || self.output_filename.is_some()
            || self.css_output_filename.is_some()
        {
            return Err("Module options support only rootDir and experimental".into());
        }
        Ok(())
    }

    pub fn apply(&self, options: &mut CompileOptions) -> Result<(), String> {
        options.root_dir.clone_from(&self.root_dir);
        options.sourcemap = self
            .sourcemap
            .as_ref()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned));
        if let Some(css) = &self.css {
            options.css = match css.as_str() {
                "external" => CssMode::External,
                "injected" => CssMode::Injected,
                _ => return Err(format!("Unsupported CSS mode: {css}")),
            };
        }
        if let Some(value) = self.preserve_comments {
            options.preserve_comments = value;
        }
        if let Some(value) = self.preserve_whitespace {
            options.preserve_whitespace = value;
        }
        if let Some(value) = self.disclose_version {
            options.disclose_version = value;
        }
        if let Some(value) = self.accessors {
            options.accessors = value;
        }
        if let Some(value) = self.immutable {
            options.immutable = value;
        }
        if let Some(value) = &self.namespace {
            options.namespace = match value.as_str() {
                "html" => Namespace::Html,
                "svg" => Namespace::Svg,
                "mathml" => Namespace::Mathml,
                _ => return Err(format!("Unsupported namespace: {value}")),
            };
        }
        options.name.clone_from(&self.name);
        options.output_filename.clone_from(&self.output_filename);
        options
            .css_output_filename
            .clone_from(&self.css_output_filename);
        if let Some(value) = &self.experimental {
            options.experimental.r#async = value.r#async;
        }
        Ok(())
    }
}

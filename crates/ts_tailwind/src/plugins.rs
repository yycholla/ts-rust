//! Native plugin registrations. Descriptors identify Rust implementations;
//! arbitrary JavaScript paths are never evaluated by the stylesheet compiler.
use crate::{
    ast::{AstNode, NodeKind, SourceLocation},
    design_system::DesignSystem,
    snapshot::PluginRegistration,
    typography::{TypographyOptions, register_typography},
};
use serde_json::{Map, Value};

#[derive(Clone, Debug)]
pub struct PluginDirective {
    pub specifier: String,
    pub options: Map<String, Value>,
    pub source: Option<SourceLocation>,
}

pub fn directive(
    specifier: &str,
    body: &[AstNode],
    source: Option<SourceLocation>,
) -> Result<PluginDirective, String> {
    let mut options = Map::new();
    for node in body {
        if matches!(node.kind, NodeKind::Comment { .. }) {
            continue;
        }
        let NodeKind::Declaration {
            property,
            value: Some(value),
            important: false,
        } = &node.kind
        else {
            return Err("Native plugin options must be declarations".into());
        };
        let value = value.trim();
        // Typography's constructor has two string options. Reject structured
        // configuration rather than converting it to a misleading string.
        if !matches!(property.as_str(), "className" | "target") {
            return Err(format!("Unsupported native Typography option {property}"));
        }
        let value = if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        if value.is_empty() || value.contains(['\\', ',', '{', '}']) {
            return Err(format!("Invalid native Typography option {property}"));
        }
        options.insert(property.clone(), Value::String(value.into()));
    }
    Ok(PluginDirective {
        specifier: specifier.into(),
        options,
        source,
    })
}

/// Atomic across every directive. Registrations occur in stylesheet order;
/// later CSS custom variants are registered after the plugins by the caller.
pub fn register(
    system: &mut DesignSystem,
    directives: &[PluginDirective],
    registrations: &[PluginRegistration],
) -> Result<(), (String, Option<SourceLocation>)> {
    let mut prepared = system.clone();
    for directive in directives {
        let result = (|| -> Result<(), String> {
            let registration = registrations
                .iter()
                .find(|item| item.specifier == directive.specifier)
                .ok_or_else(|| {
                    format!("No native plugin registration for {}", directive.specifier)
                })?;
            if registration.implementation != "@tailwindcss/typography" {
                return Err(format!(
                    "Unsupported native plugin implementation {}",
                    registration.implementation
                ));
            }
            let mut options = registration
                .options
                .as_object()
                .cloned()
                .unwrap_or_default();
            options.extend(directive.options.clone());
            let options: TypographyOptions = serde_json::from_value(Value::Object(options))
                .map_err(|error| format!("Invalid Typography options: {error}"))?;
            register_typography(&mut prepared, &registration.version, &options, None)
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Err(error) = result {
            return Err((error, directive.source.clone()));
        }
    }
    *system = prepared;
    Ok(())
}

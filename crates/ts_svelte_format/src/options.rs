use crate::FormatError;
use oxc_formatter::{ArrowParentheses, QuoteProperties, QuoteStyle, Semicolons, TrailingCommas};
use rsvelte_formatter::{
    CssFormatOptions, CssSingleQuote, CssTrailingCommas, FormatOptions, IndentStyle, IndentWidth,
    JsFormatOptions, JsonFormatOptions, LineWidth, SortOrderSpec,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Resolved per-file settings. Hosts resolve presets/overrides before calling;
/// unknown fields are errors, including plugins and executable configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ResolvedOptions {
    pub print_width: u16,
    pub tab_width: u8,
    pub use_tabs: bool,
    pub single_quote: bool,
    pub semi: bool,
    pub trailing_comma: TrailingComma,
    pub quote_props: QuoteProps,
    pub arrow_parens: ArrowParens,
    pub bracket_spacing: bool,
    pub bracket_same_line: bool,
    pub single_attribute_per_line: bool,
    pub end_of_line: EndOfLine,
    pub sort_imports: bool,
    pub svelte_allow_shorthand: bool,
    pub svelte_indent_script_and_style: bool,
    pub svelte_sort_order: String,
}

impl Default for ResolvedOptions {
    fn default() -> Self {
        Self {
            print_width: 80,
            tab_width: 2,
            use_tabs: false,
            single_quote: false,
            semi: true,
            trailing_comma: TrailingComma::All,
            quote_props: QuoteProps::AsNeeded,
            arrow_parens: ArrowParens::Always,
            bracket_spacing: true,
            bracket_same_line: false,
            single_attribute_per_line: false,
            end_of_line: EndOfLine::Lf,
            sort_imports: false,
            svelte_allow_shorthand: true,
            svelte_indent_script_and_style: true,
            svelte_sort_order: "options-scripts-markup-styles".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrailingComma {
    All,
    Es5,
    None,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteProps {
    AsNeeded,
    Consistent,
    Preserve,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArrowParens {
    Always,
    Avoid,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndOfLine {
    Lf,
    Crlf,
    Cr,
    Auto,
}

pub(crate) struct BackendOptions {
    pub svelte: FormatOptions,
    pub css: CssFormatOptions,
    pub json: JsonFormatOptions,
}

impl ResolvedOptions {
    /// Validate numeric and Svelte ordering options without formatting source.
    pub fn validate(&self) -> Result<(), FormatError> {
        self.backend().map(|_| ())
    }

    pub(crate) fn backend(&self) -> Result<BackendOptions, FormatError> {
        let indent_width = IndentWidth::try_from(self.tab_width).map_err(|_| {
            FormatError::new(
                "invalid_options",
                "tabWidth is outside the formatter's supported range",
            )
        })?;
        let line_width = LineWidth::try_from(self.print_width).map_err(|_| {
            FormatError::new(
                "invalid_options",
                "printWidth is outside the formatter's supported range",
            )
        })?;
        let sort_order = SortOrderSpec::parse(&self.svelte_sort_order)
            .ok_or_else(|| FormatError::new("invalid_options", "Invalid svelteSortOrder"))?;
        let indent_style = if self.use_tabs {
            IndentStyle::Tab
        } else {
            IndentStyle::Space
        };
        // Format uniformly in LF, then apply the requested file line ending.
        // This also covers Svelte markup, whose printer is LF internally.
        let js = JsFormatOptions {
            indent_style,
            indent_width,
            line_width,
            quote_style: if self.single_quote {
                QuoteStyle::Single
            } else {
                QuoteStyle::Double
            },
            semicolons: if self.semi {
                Semicolons::Always
            } else {
                Semicolons::AsNeeded
            },
            trailing_commas: match self.trailing_comma {
                TrailingComma::All => TrailingCommas::All,
                TrailingComma::Es5 => TrailingCommas::Es5,
                TrailingComma::None => TrailingCommas::None,
            },
            quote_properties: match self.quote_props {
                QuoteProps::AsNeeded => QuoteProperties::AsNeeded,
                QuoteProps::Consistent => QuoteProperties::Consistent,
                QuoteProps::Preserve => QuoteProperties::Preserve,
            },
            arrow_parentheses: match self.arrow_parens {
                ArrowParens::Always => ArrowParentheses::Always,
                ArrowParens::Avoid => ArrowParentheses::AsNeeded,
            },
            bracket_spacing: self.bracket_spacing.into(),
            bracket_same_line: self.bracket_same_line.into(),
            sort_imports: self
                .sort_imports
                .then(rsvelte_formatter::SortImportsOptions::default),
            ..JsFormatOptions::default()
        };
        let css = CssFormatOptions {
            indent_style,
            indent_width,
            line_width,
            single_quote: CssSingleQuote::from(self.single_quote),
            trailing_commas: match self.trailing_comma {
                TrailingComma::None => CssTrailingCommas::Never,
                _ => CssTrailingCommas::Always,
            },
            ..CssFormatOptions::default()
        };
        let json = JsonFormatOptions {
            indent_style,
            indent_width,
            line_width,
            bracket_spacing: self.bracket_spacing.into(),
            ..JsonFormatOptions::default()
        };
        // Upstream native_style_formatter preserves invalid CSS silently. Our
        // contract instead propagates errors and never emits a partial edit.
        let style_formatter = Arc::new(move |source: &str, lang: &str, width: usize| {
            let mut options = css;
            options.line_width = LineWidth::try_from(u16::try_from(width).unwrap_or(u16::MAX))
                .map_err(|_| "Embedded CSS print width is invalid".to_owned())?;
            let dialect = crate::css_dialect(lang).map_err(|error| error.message)?;
            rsvelte_formatter::format_css_source(source, dialect, &options)
                .map_err(|error| error.to_string())
        });
        let svelte = FormatOptions {
            js,
            style_formatter: Some(style_formatter),
            attributes: rsvelte_formatter::AttributeFormatOptions {
                single_attribute_per_line: self.single_attribute_per_line,
                allow_shorthand: self.svelte_allow_shorthand,
            },
            indent_script_and_style: self.svelte_indent_script_and_style,
            bracket_same_line: self.bracket_same_line,
            sort_order,
            ..FormatOptions::default()
        };
        Ok(BackendOptions { svelte, css, json })
    }

    pub(crate) fn line_ending(&self, source: &str) -> &'static str {
        match self.end_of_line {
            EndOfLine::Lf => "\n",
            EndOfLine::Crlf => "\r\n",
            EndOfLine::Cr => "\r",
            EndOfLine::Auto => match source
                .as_bytes()
                .iter()
                .position(|b| matches!(b, b'\r' | b'\n'))
            {
                Some(i)
                    if source.as_bytes()[i] == b'\r'
                        && source.as_bytes().get(i + 1) == Some(&b'\n') =>
                {
                    "\r\n"
                }
                Some(i) if source.as_bytes()[i] == b'\r' => "\r",
                _ => "\n",
            },
        }
    }
}

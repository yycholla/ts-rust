//! Shared, mutable native design-system registrations. Custom utilities are
//! ordered generators, not replacements for built-ins with the same root.
use crate::{
    ast::AstNode,
    candidate::{Candidate, CandidateKind, CandidateRegistry, Variant},
    custom_utility::{CustomUtility, CustomUtilityResult},
    theme::Theme,
    utilities::{GeneratedUtility, Utilities, UtilityResult},
    variants::{VariantError, Variants},
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UtilityId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CandidateTransformErrorKind {
    Invalid,
    Unsupported,
    /// Resource exhaustion must fail the operation, not discard a candidate.
    Limit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateTransformError {
    pub kind: CandidateTransformErrorKind,
    pub message: String,
}
pub type CandidateTransform =
    fn(&[AstNode], &Theme) -> Result<Vec<AstNode>, CandidateTransformError>;
/// Native plugin callback, evaluated per candidate before property ordering.
pub type UtilityTransform = fn(&Candidate, &[AstNode], &Theme) -> Result<Vec<AstNode>, String>;

#[derive(Clone)]
pub struct DesignSystem {
    theme: Theme,
    utilities: Utilities,
    variants: Variants,
    definitions: Vec<(UtilityId, CustomUtility)>,
    by_root: BTreeMap<String, Vec<usize>>,
    excluded: BTreeSet<String>,
    transform: Option<CandidateTransform>,
    utility_transforms: BTreeMap<UtilityId, UtilityTransform>,
}
#[derive(Debug, Clone, Default)]
pub struct BaseCompilation {
    pub utilities: Vec<GeneratedUtility>,
    pub unsupported: Vec<String>,
}
impl DesignSystem {
    pub fn new(theme: Theme) -> Self {
        let variants = Variants::new(&theme);
        Self {
            theme,
            utilities: Utilities::new(),
            variants,
            definitions: Vec::new(),
            by_root: BTreeMap::new(),
            excluded: BTreeSet::new(),
            transform: None,
            utility_transforms: BTreeMap::new(),
        }
    }
    pub fn theme(&self) -> &Theme {
        &self.theme
    }
    pub fn mark_used_variable(&mut self, name: &str) -> bool {
        self.theme.mark_used_variable(name)
    }
    pub fn variants(&self) -> &Variants {
        &self.variants
    }
    /// Custom variant registration can be added to Variants without replacing
    /// this shared registry or the compiler. No callback is executed here.
    pub fn variants_mut(&mut self) -> &mut Variants {
        &mut self.variants
    }
    pub fn exclude(&mut self, candidate: String) {
        self.excluded.insert(candidate);
    }
    pub fn is_excluded(&self, candidate: &str) -> bool {
        self.excluded.contains(candidate)
    }
    pub fn set_candidate_transform(&mut self, transform: CandidateTransform) {
        self.transform = Some(transform);
    }
    pub fn clear_candidate_transform(&mut self) {
        self.transform = None;
    }
    pub fn transform_candidate(
        &self,
        nodes: Vec<AstNode>,
    ) -> Result<Vec<AstNode>, CandidateTransformError> {
        match self.transform {
            Some(transform) => transform(&nodes, &self.theme),
            None => Ok(nodes),
        }
    }
    pub fn register_utility(&mut self, definition: AstNode) -> Result<UtilityId, String> {
        if self.definitions.len() >= 16_384 {
            return Err("Custom utility registration limit exceeded".into());
        }
        let utility = CustomUtility::from_ast(definition)?;
        let index = self.definitions.len();
        let id = UtilityId(index as u64);
        self.by_root
            .entry(utility.root().into())
            .or_default()
            .push(index);
        self.definitions.push((id, utility));
        Ok(id)
    }
    pub fn register_utility_with_transform(
        &mut self,
        definition: AstNode,
        transform: UtilityTransform,
    ) -> Result<UtilityId, String> {
        let id = self.register_utility(definition)?;
        self.utility_transforms.insert(id, transform);
        Ok(id)
    }
    pub fn utility(&self, id: UtilityId) -> Option<&CustomUtility> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.definitions.get(index))
            .filter(|(found, _)| *found == id)
            .map(|(_, utility)| utility)
    }
    pub fn definitions(&self) -> impl Iterator<Item = (UtilityId, &CustomUtility)> {
        self.definitions.iter().map(|(id, utility)| (*id, utility))
    }
    pub fn definitions_for_root(
        &self,
        root: &str,
    ) -> impl Iterator<Item = (UtilityId, &CustomUtility)> {
        self.by_root.get(root).into_iter().flatten().map(|index| {
            let (id, utility) = &self.definitions[*index];
            (*id, utility)
        })
    }
    /// Failed preparation preserves the old body. IDs and registration order are
    /// stable so @apply dependency graphs need no pointer identity emulation.
    pub fn replace_utility_body(
        &mut self,
        id: UtilityId,
        body: Vec<AstNode>,
    ) -> Result<(), String> {
        let index = usize::try_from(id.0).map_err(|_| "Unknown custom utility identity")?;
        let (_, utility) = self
            .definitions
            .get_mut(index)
            .filter(|(found, _)| *found == id)
            .ok_or("Unknown custom utility identity")?;
        utility.replace_body(body)
    }
    pub fn registry(&self) -> CandidateRegistry {
        let mut registry = self.utilities.candidate_registry(&self.theme);
        self.variants.register_candidates(&mut registry);
        for (_, utility) in &self.definitions {
            if utility.functional() {
                registry.functional_utilities.insert(utility.root().into());
            } else {
                registry.static_utilities.insert(utility.root().into());
            }
        }
        registry
    }
    pub fn compare_variants(
        &self,
        a: Option<&Variant>,
        b: Option<&Variant>,
    ) -> Result<Ordering, VariantError> {
        self.variants.compare(a, b)
    }
    pub fn apply_variant(
        &self,
        variant: &Variant,
        nodes: Vec<AstNode>,
    ) -> Result<Vec<AstNode>, VariantError> {
        self.variants.apply(variant, nodes)
    }
    pub fn compile_base(&self, candidate: &Candidate) -> Result<BaseCompilation, String> {
        let mut output = BaseCompilation::default();
        if self.is_excluded(&candidate.raw) {
            return Ok(output);
        }
        match self.utilities.compile_base(candidate, &self.theme) {
            UtilityResult::Generated { utility } => output.utilities.push(utility),
            UtilityResult::InvalidCandidate => {}
            UtilityResult::Unsupported { root, reason } => {
                output.unsupported.push(format!("{root}: {reason}"))
            }
        }
        let root = match &candidate.kind {
            CandidateKind::Static { root } | CandidateKind::Functional { root, .. } => root,
            _ => return Ok(output),
        };
        for (id, definition) in self.definitions_for_root(root) {
            match definition.compile(candidate, &self.theme)? {
                CustomUtilityResult::Generated(nodes) => {
                    let nodes = match self.utility_transforms.get(&id) {
                        Some(transform) => transform(candidate, &nodes, &self.theme)?,
                        None => nodes,
                    };
                    let (property_order, declaration_count) = self.utilities.property_sort(&nodes);
                    output.utilities.push(GeneratedUtility {
                        nodes,
                        property_order,
                        declaration_count,
                    });
                }
                CustomUtilityResult::NoMatch => {}
                CustomUtilityResult::Stop => break,
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
#[path = "design_system_tests.rs"]
mod tests;

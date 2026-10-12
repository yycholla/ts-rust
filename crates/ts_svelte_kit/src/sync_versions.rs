//! Explicit Kit sync contracts, derived from the locally pinned compiler
//! packages. Unknown versions are rejected instead of guessed compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitSyncVersion {
    /// 2.67.0, 2.68.0 and 2.69.2 share the same write_types source.
    Kit2LegacyExports,
    /// 2.69.3 and 2.70.3 recognize destructured load/action exports.
    Kit2DestructuredExports,
    /// 3.0.1 and 3.0.0-next.32 share the same write_types source.
    Kit3,
    Kit3Prerelease,
}

impl KitSyncVersion {
    pub fn parse(version: &str) -> Result<Self, String> {
        match version {
            "2.67.0" | "2.68.0" | "2.69.2" => Ok(Self::Kit2LegacyExports),
            "2.69.3" | "2.70.3" => Ok(Self::Kit2DestructuredExports),
            "3.0.1" => Ok(Self::Kit3),
            "3.0.0-next.32" => Ok(Self::Kit3Prerelease),
            _ => Err(format!(
                "Unsupported native Kit sync version {version}; registered contracts: 2.67.0, 2.68.0, 2.69.2, 2.69.3, 2.70.3, 3.0.1, 3.0.0-next.32"
            )),
        }
    }

    pub fn is_kit3(self) -> bool {
        matches!(self, Self::Kit3 | Self::Kit3Prerelease)
    }

    pub fn destructured_exports(self) -> bool {
        self != Self::Kit2LegacyExports
    }
}

impl Default for KitSyncVersion {
    fn default() -> Self {
        Self::Kit2LegacyExports
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_registered_contracts_do_not_guess_future_versions() {
        assert!(
            !KitSyncVersion::parse("2.69.2")
                .unwrap()
                .destructured_exports()
        );
        assert!(
            KitSyncVersion::parse("2.69.3")
                .unwrap()
                .destructured_exports()
        );
        assert!(KitSyncVersion::parse("3.0.0-next.32").unwrap().is_kit3());
        assert!(KitSyncVersion::parse("3.0.1").unwrap().is_kit3());
        assert!(KitSyncVersion::parse("3.1.0").is_err());
    }
}

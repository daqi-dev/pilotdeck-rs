//! #988: fixture-only switches; ordinary and release libraries cannot enable them.

#[cfg(all(test, feature = "pdrs-mutants"))]
pub fn on(name: &str) -> bool {
    !name.is_empty() && std::env::var("PDRS_MUTANT").is_ok_and(|selected| selected == name)
}

#[cfg(not(all(test, feature = "pdrs-mutants")))]
#[inline(always)]
pub const fn on(_name: &str) -> bool {
    false
}

#[cfg(all(test, not(feature = "pdrs-mutants")))]
mod tests {
    #[test]
    fn switches_are_disabled_without_feature() {
        const DISABLED: bool = super::on("default-root-dir");
        assert!(
            !std::hint::black_box(DISABLED),
            "#988 non-feature switch must be constant false"
        );
        assert!(!super::on(""), "#988 empty selection must be disabled");
    }
}

#[cfg(all(test, feature = "pdrs-mutants"))]
mod tests {
    #[test]
    fn selection_matches_only_the_requested_name() {
        let selected = std::env::var("PDRS_MUTANT").unwrap_or_default();
        assert_eq!(
            super::on(&selected),
            !selected.is_empty(),
            "#988 selected switch"
        );
        assert!(!super::on(&(selected + "-other")), "#988 unrelated switch");
        assert!(!super::on(""), "#988 empty switch");
    }
}

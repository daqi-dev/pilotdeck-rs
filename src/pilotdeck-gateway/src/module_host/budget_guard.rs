//! #1914: the #1696 budget guards, written once. The module-host (#1349)
//! and approvals (#1350) replay lanes each carried a line-for-line copy of
//! this block; the only deltas between the copies were the issue label in
//! the messages, the transport budget the guards derive from, the four
//! switch prefixes, and one doc phrase — exactly the deltas [`Lane`]
//! carries. The per-lane wrappers and their seven unit tests come from
//! [`budget_guard_lane!`], so the helper names, test paths and assertion
//! anchors the negative-control fixtures bind are unchanged (only the
//! fixtures' `path` columns move to this file).
use serde_json::Value;

/// The lane a guard run serves: everything the two copies disagreed on.
pub(super) enum Lane {
    ModuleHost,
    Approvals,
}

impl Lane {
    /// The issue label every guard message carries (`#1349` / `#1350`).
    pub(super) const fn issue(&self) -> &'static str {
        match self {
            Lane::ModuleHost => "#1349",
            Lane::Approvals => "#1350",
        }
    }

    /// The transport's own input budget, the ceiling the pad guard derives
    /// from (`INPUT_BUDGET` in `module_host.rs` / `approvals.rs`).
    pub(super) const fn budget(&self) -> usize {
        match self {
            Lane::ModuleHost => super::INPUT_BUDGET,
            Lane::Approvals => super::approvals::INPUT_BUDGET,
        }
    }

    /// #1696: the ceiling is the point of the guard, so the mutation switch
    /// restores the old unbounded read rather than the field check.
    pub(super) fn pad_size_bypassed(&self) -> bool {
        match self {
            Lane::ModuleHost => crate::mutants::on("module-host-oracle-pad-size"),
            Lane::Approvals => crate::mutants::on("approvals-oracle-pad-size"),
        }
    }

    /// #1696: same shape as `pad_size` — the switch restores the old
    /// unchecked read rather than the field checks.
    pub(super) fn pad_marker_bypassed(&self) -> bool {
        match self {
            Lane::ModuleHost => crate::mutants::on("module-host-oracle-pad-marker"),
            Lane::Approvals => crate::mutants::on("approvals-oracle-pad-marker"),
        }
    }

    pub(super) fn boundary_body_length_bypassed(&self) -> bool {
        match self {
            Lane::ModuleHost => crate::mutants::on("module-host-oracle-boundary-body-length"),
            Lane::Approvals => crate::mutants::on("approvals-oracle-boundary-body-length"),
        }
    }

    pub(super) fn unknown_budget_cell_bypassed(&self) -> bool {
        match self {
            Lane::ModuleHost => crate::mutants::on("module-host-oracle-unknown-budget-cell"),
            Lane::Approvals => crate::mutants::on("approvals-oracle-unknown-budget-cell"),
        }
    }
}

/// Pin each budget cell's *expanded* body to the budget it exists to probe
/// (#1696). The pad syntax was already fail-closed, but its size was
/// unconstrained: `budget-exact` / `budget-over` answer a short body exactly
/// as they answer the boundary body (module-host: a served response / the
/// 413; approvals: a served snapshot / a zero-byte close), so an oracle
/// edited to a tiny pad — or a generator that recorded the wrong size —
/// would leave both cells green while proving nothing. The body must
/// therefore be the length the cell claims to test, in the transport's own
/// `INPUT_BUDGET` units. Unknown `budget-*` ids fail closed: a new boundary
/// cell must be taught its length here (the switch exists so the negative
/// control can prove the guard rejects it), otherwise it would replay
/// without any length pin — the same silent pass this guard removes.
pub(super) fn budget_body_len_checked(lane: Lane, id: &str, request: &[u8]) -> Result<(), String> {
    let issue = lane.issue();
    let expected = match id.strip_prefix("budget-") {
        Some("exact") => lane.budget(),
        Some("over") => lane.budget() + 1,
        Some(class) => {
            if lane.unknown_budget_cell_bypassed() {
                return Ok(());
            }
            return Err(format!(
                "{issue} cell {id}: budget cell class `{class}` has no length guard; teach \
                 `budget_body_len_checked` its expected body length before adding it"
            ));
        }
        None => return Ok(()),
    };
    let head = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| format!("{issue} cell {id} has no header terminator"))?
        + 4;
    let len = request.len() - head;
    if len != expected && !lane.boundary_body_length_bypassed() {
        return Err(format!(
            "{issue} cell {id}: the expanded body must be {expected} bytes (the transport \
             INPUT_BUDGET), not {len}; the fixture and `INPUT_BUDGET` have drifted apart"
        ));
    }
    Ok(())
}

/// The recorded pad size, bounded before `"a".repeat(size)` can abort the
/// whole test binary on a huge allocation (#1696).
pub(super) fn pad_size_checked(lane: Lane, id: &str, entry: &Value) -> Result<usize, String> {
    let issue = lane.issue();
    let size = entry["pad"]["size"]
        .as_u64()
        .ok_or_else(|| format!("{issue} cell {id} pad size must be a u64"))?;
    let size = usize::try_from(size)
        .map_err(|_| format!("{issue} cell {id} pad size {size} does not fit usize"))?;
    // #1696: the ceiling is the point of the guard, so the mutation switch
    // restores the old unbounded read rather than the field check.
    let budget = lane.budget();
    if size > budget && !lane.pad_size_bypassed() {
        return Err(format!(
            "{issue} cell {id} pad size {size} is beyond the replay budget {budget}"
        ));
    }
    Ok(size)
}

/// #1696: the pad marker must be non-empty and appear exactly once. An empty
/// marker makes `str::replace` splice the pad between every character, and a
/// repeated marker multiplies the padding — either way the allocation, not
/// the recording, would decide the replay.
pub(super) fn pad_marker_checked<'a>(
    lane: Lane,
    id: &str,
    entry: &'a Value,
    request: &str,
) -> Result<&'a str, String> {
    let issue = lane.issue();
    let marker = entry["pad"]["marker"]
        .as_str()
        .ok_or_else(|| format!("{issue} cell {id} pad marker must be a string"))?;
    // #1696: same shape as `pad_size_checked` — the switch restores the old
    // unchecked read rather than the field checks.
    if lane.pad_marker_bypassed() {
        return Ok(marker);
    }
    if marker.is_empty() {
        return Err(format!("{issue} cell {id} pad marker must not be empty"));
    }
    let hits = request.matches(marker).count();
    if hits != 1 {
        return Err(format!(
            "{issue} cell {id} pad marker appears {hits} times in its request, expected exactly 1"
        ));
    }
    Ok(marker)
}

/// One lane's guard wrappers plus their seven unit tests (#1914): the shared
/// bodies live in this file, and the generated items keep the helper names,
/// test paths and assertion anchors the negative-control fixtures bind, so
/// the fixtures' `test` columns do not move. `$cell` is the lane's replay
/// `Cell` type, read by `assert_budget_body_len`.
macro_rules! budget_guard_lane {
    ($lane:ident, $cell:ty) => {
        /// Allocation ceiling for a fixture-supplied pad size (#1696). The
        /// largest pad the oracle legitimately records is the `budget-over`
        /// body's, which stays below the transport's budget; anything past
        /// `INPUT_BUDGET` is fixture drift and is refused *before*
        /// `"a".repeat(size)` can abort the whole test binary on allocation
        /// instead of failing one assertion — the failure mode #1606 fix-r1
        /// removed on the api-server side with `REPLAY_BODY_BUDGET`. It is
        /// derived from the product budget on purpose: a fixture that stops
        /// tracking the budget is the drift this guard exists to catch.
        const REPLAY_PAD_BUDGET: usize =
            $crate::module_host::budget_guard::Lane::$lane.budget();

        fn budget_body_len_checked(id: &str, request: &[u8]) -> Result<(), String> {
            $crate::module_host::budget_guard::budget_body_len_checked(
                $crate::module_host::budget_guard::Lane::$lane,
                id,
                request,
            )
        }

        fn assert_budget_body_len(cell: &$cell) {
            budget_body_len_checked(&cell.id, &cell.request)
                .unwrap_or_else(|error| panic!("{error}"));
        }

        fn pad_size_checked(id: &str, entry: &::serde_json::Value) -> Result<usize, String> {
            $crate::module_host::budget_guard::pad_size_checked(
                $crate::module_host::budget_guard::Lane::$lane,
                id,
                entry,
            )
        }

        fn pad_size(id: &str, entry: &::serde_json::Value) -> usize {
            pad_size_checked(id, entry).unwrap_or_else(|error| panic!("{error}"))
        }

        fn pad_marker_checked<'a>(
            id: &str,
            entry: &'a ::serde_json::Value,
            request: &str,
        ) -> Result<&'a str, String> {
            $crate::module_host::budget_guard::pad_marker_checked(
                $crate::module_host::budget_guard::Lane::$lane,
                id,
                entry,
                request,
            )
        }

        fn pad_marker<'a>(
            id: &str,
            entry: &'a ::serde_json::Value,
            request: &str,
        ) -> &'a str {
            pad_marker_checked(id, entry, request).unwrap_or_else(|error| panic!("{error}"))
        }

        #[test]
        fn recorded_pad_size_rejects_drift() {
            $crate::negative_assert!(
                pad_size_checked(
                    "budget-exact",
                    &::serde_json::json!({"pad":{"size": (REPLAY_PAD_BUDGET as u64) + 1}}),
                )
                .is_err(),
                "{} #1696 rejects pad size drift",
                $crate::module_host::budget_guard::Lane::$lane.issue()
            );
        }

        #[test]
        #[should_panic(expected = "is beyond the replay budget")]
        fn recorded_pad_size_panics_on_drift() {
            pad_size(
                "budget-exact",
                &::serde_json::json!({"pad":{"size": u64::MAX}}),
            );
        }

        #[test]
        fn recorded_pad_marker_rejects_drift() {
            let request = "POST / HTTP/1.1\r\n\r\nAAA";
            $crate::negative_assert!(
                pad_marker_checked(
                    "budget-exact",
                    &::serde_json::json!({"pad":{"marker": ""}}),
                    request
                )
                .is_err(),
                "{} #1696 rejects pad marker drift",
                $crate::module_host::budget_guard::Lane::$lane.issue()
            );
            $crate::negative_assert!(
                pad_marker_checked(
                    "budget-exact",
                    &::serde_json::json!({"pad":{"marker": "A"}}),
                    request
                )
                .is_err(),
                "{} #1696 rejects pad marker drift",
                $crate::module_host::budget_guard::Lane::$lane.issue()
            );
            $crate::negative_assert!(
                pad_marker_checked(
                    "budget-exact",
                    &::serde_json::json!({"pad":{"marker": "__PDRS_PAD__"}}),
                    request
                )
                .is_err(),
                "{} #1696 rejects pad marker drift",
                $crate::module_host::budget_guard::Lane::$lane.issue()
            );
        }

        #[test]
        #[should_panic(expected = "pad marker must not be empty")]
        fn recorded_pad_marker_panics_on_drift() {
            pad_marker(
                "budget-exact",
                &::serde_json::json!({"pad":{"marker": ""}}),
                "POST / HTTP/1.1\r\n\r\n",
            );
        }

        #[test]
        fn recorded_budget_body_length_rejects_drift() {
            $crate::negative_assert!(
                budget_body_len_checked("budget-exact", b"POST / HTTP/1.1\r\n\r\nshort").is_err(),
                "{} #1696 rejects boundary body length drift",
                $crate::module_host::budget_guard::Lane::$lane.issue()
            );
        }

        #[test]
        #[should_panic(expected = "the expanded body must be")]
        fn recorded_budget_body_length_panics_on_drift() {
            budget_body_len_checked("budget-exact", b"POST / HTTP/1.1\r\n\r\nshort")
                .unwrap_or_else(|error| panic!("{error}"));
        }

        #[test]
        fn recorded_unknown_budget_cell_is_rejected() {
            $crate::negative_assert!(
                budget_body_len_checked("budget-under", b"POST / HTTP/1.1\r\n\r\n").is_err(),
                "{} #1696 fails closed on a budget cell class with no length guard",
                $crate::module_host::budget_guard::Lane::$lane.issue()
            );
            // Non-budget cells are not this guard's business.
            assert!(budget_body_len_checked("served", b"POST / HTTP/1.1\r\n\r\n").is_ok());
        }
    };
}

pub(super) use budget_guard_lane;

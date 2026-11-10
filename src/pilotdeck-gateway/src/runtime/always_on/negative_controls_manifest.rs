//! #2488: the closed-set pin for the always-on-2294 negative controls.
//!
//! The fixture (`tests/fixtures/always-on-2294/negative_controls.py`) is
//! the kill surface for the port's mutation rows, and its `--check` guards
//! the hand-copied manifest numbers against `oracle.json` and this module's
//! sources — but only a fresh full run proves a row still goes KILLED, and
//! that needs cargo plus a pristine tree (the coordinator's host chain).
//! A row silently deleted from the table would therefore shrink the kill
//! surface without any machine noticing. This module is the gate that
//! notices: it pins the fixture's bytes (any edit re-pins the digest) and
//! the table's row names, in order, as a closed set — the always-on twin
//! of the session-assembly-1316 pin (`test_negative_control_table_cannot_
//! shrink_silently`, held in the metadata driver's module there).
//!
//! The roster this pins is the #2566/#2488 set: the five prev-plan §5 rows
//! plus the re-arm row bound to `a_failed_watch_start_re_arms_after_the_
//! root_returns`.

use sha2::Digest;

/// The fixture this module pins, bytewise at compile time.
const CONTROLS: &str = include_str!("../../../tests/fixtures/always-on-2294/negative_controls.py");

/// The digest of [`CONTROLS`]: any fixture edit — even one that keeps the
/// row list intact — must re-pin this consciously, next to the row-list
/// change that explains it.
const CONTROLS_SHA256: &str = "57b3416f7a33a7f65d0ae52001462483dfa1dca8e34e3784242279e9ca7faa72";

/// The closed set itself: every mutation row's name, in the order the
/// table holds it. Shrink, reorder and rename all fail the test below;
/// growth fails the digest above until this list grows with it.
const ROWS: [&str; 6] = [
    "gate-chain-lock-gate-removed",
    "stop-order-swapped",
    "reload-rebuilds-unchanged-family",
    "advertise-without-manager",
    "fired-tick-keeps-the-lock",
    "watch-start-before-slot-store",
];

/// The row names of the fixture's `mutations = [...]` table, in file order:
/// the block from its opening line to the first bare `]`, one name per
/// `    ('name',` row start. A bounded line scan — the table is the only
/// place the fixture spells that shape.
fn table_rows(source: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut inside = false;
    for line in source.lines() {
        if inside {
            if line == "]" {
                break;
            }
            if let Some(rest) = line.strip_prefix("    ('")
                && let Some((name, _)) = rest.split_once('\'')
            {
                rows.push(name.to_owned());
            }
        } else if line == "mutations = [" {
            inside = true;
        }
    }
    rows
}

/// #2488: the fixture bytes and its row table form a pinned closed set —
/// a row dropped from the table (or reordered, renamed) is a silent shrink
/// of the kill surface, and any fixture edit at all must re-pin the digest.
#[test]
fn the_negative_control_table_cannot_shrink_silently() {
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(CONTROLS.as_bytes())),
        CONTROLS_SHA256,
        "#2488 the fixture bytes must match the pinned digest; re-pin it \
         next to the row-list change that explains the edit"
    );
    assert_eq!(
        table_rows(CONTROLS),
        ROWS,
        "#2488 the mutation table must hold exactly the pinned rows, in order"
    );
    // The proof the pin bites: the same parse applied to a table with a row
    // dropped, renamed, or reordered must not equal the pinned set.
    for row in [
        "gate-chain-lock-gate-removed",
        "watch-start-before-slot-store",
    ] {
        let opening = format!("    ('{row}', ");
        let dropped = CONTROLS.replacen(
            &format!("{opening}{}\n", row_opening_tail(CONTROLS, &opening)),
            "",
            1,
        );
        assert_ne!(
            table_rows(&dropped),
            ROWS,
            "#2488 a dropped row must diverge"
        );
    }
    let renamed = CONTROLS.replacen("('stop-order-swapped',", "('stop-order-swapped-x',", 1);
    assert_ne!(
        table_rows(&renamed),
        ROWS,
        "#2488 a renamed row must diverge"
    );
    // The two openings are not adjacent lines (each row carries its own
    // anchor bytes between them), so swap through a placeholder that cannot
    // collide with fixture text.
    let first = "    ('gate-chain-lock-gate-removed', gates,\n";
    let second = "    ('stop-order-swapped', automation,\n";
    let reordered = CONTROLS
        .replacen(first, "\u{0}holder\n", 1)
        .replacen(second, first, 1)
        .replacen("\u{0}holder\n", second, 1);
    assert_ne!(
        table_rows(&reordered),
        ROWS,
        "#2488 a reordered table must diverge"
    );
}

/// The rest of a row's opening line (from the name's closing quote to the
/// line end), so the drop proof deletes the whole opening line rather than
/// a name someone hand-wrote into the proof.
fn row_opening_tail(source: &str, opening: &str) -> String {
    let start = source.find(opening).expect("row opening present");
    let line = source[start..].lines().next().expect("row opening line");
    line.strip_prefix(opening)
        .expect("opening is the line's prefix")
        .to_owned()
}

// Port of yaml@2.9.0 dist/compose/util-empty-scalar-position.js (ISC).
use crate::parse::cst::{Token, TokenType::*};
pub(super) fn empty_scalar_position(mut offset: usize, before: &[Token]) -> usize {
    for i in (0..before.len()).rev() {
        let st = &before[i];
        if matches!(st.kind, Space | Comment | Newline) {
            offset = offset.saturating_sub(st.source.as_ref().unwrap().len_utf16());
            continue;
        }
        for st in before.iter().skip(i + 1) {
            if st.kind != Space {
                break;
            }
            offset += st.source.as_ref().unwrap().len_utf16();
        }
        break;
    }
    offset
}

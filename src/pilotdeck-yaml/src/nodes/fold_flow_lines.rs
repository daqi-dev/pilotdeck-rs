// Port of yaml@2.9.0 dist/stringify/foldFlowLines.js (ISC), flow/quoted modes
// required by addPairToJSMap's collection-key coercion.
use pilotdeck_protocol::JsString;
pub(super) fn fold_flow_lines(
    text: &JsString,
    indent: usize,
    quoted: bool,
    indent_at_start: Option<usize>,
) -> JsString {
    let s = text.units();
    let end_step = (81isize - indent as isize).max(21);
    if s.len() as isize <= end_step {
        return text.clone();
    }
    let mut folds: Vec<isize> = Vec::new();
    let mut escaped = std::collections::HashSet::new();
    let mut end = 80isize - indent as isize;
    if let Some(start) = indent_at_start {
        if start > 60 {
            folds.push(0);
        } else {
            end = 80 - start as isize;
        }
    }
    let mut split = None;
    let mut prev = None;
    let mut i = -1isize;
    let mut esc_start = -1;
    let mut esc_end = -1;
    loop {
        i += 1;
        let Some(mut ch) = s.get(i as usize).copied() else {
            break;
        };
        if quoted && ch == 92 {
            esc_start = i;
            i += match s.get(i as usize + 1) {
                Some(120) => 3,
                Some(117) => 5,
                Some(85) => 9,
                _ => 1,
            };
            esc_end = i;
        }
        if ch == 10 {
            end = i + indent as isize + end_step;
            split = None;
        } else {
            if ch == 32
                && prev.is_some_and(|c| !matches!(c, 32 | 10 | 9))
                && s.get(i as usize + 1)
                    .is_some_and(|c| !matches!(c, 32 | 10 | 9))
            {
                split = Some(i);
            }
            if i >= end {
                if let Some(at) = split.filter(|i| *i != 0) {
                    folds.push(at);
                    end = at + end_step;
                    split = None;
                } else if quoted {
                    while matches!(prev, Some(32 | 9)) {
                        prev = Some(ch);
                        i += 1;
                        if let Some(c) = s.get(i as usize) {
                            ch = *c;
                        } else {
                            break;
                        }
                    }
                    let j = if i > esc_end + 1 {
                        i - 2
                    } else {
                        esc_start - 1
                    };
                    if !escaped.insert(j) {
                        return text.clone();
                    }
                    folds.push(j);
                    end = j + end_step;
                    split = None;
                }
            }
        }
        prev = Some(ch);
    }
    if folds.is_empty() {
        return text.clone();
    }
    let first = folds[0].max(0) as usize;
    let mut res = s[..first.min(s.len())].to_vec();
    for (i, fold) in folds.iter().enumerate() {
        let fold = (*fold).max(0) as usize;
        let end = folds.get(i + 1).copied().unwrap_or(s.len() as isize).max(0) as usize;
        if fold == 0 {
            res.clear();
            res.push(10);
            res.extend(vec![32; indent]);
            res.extend_from_slice(&s[..end.min(s.len())]);
        } else {
            if quoted && escaped.contains(&(fold as isize)) {
                if let Some(c) = s.get(fold) {
                    res.push(*c);
                }
                res.push(92);
            }
            res.push(10);
            res.extend(vec![32; indent]);
            let a = (fold + 1).min(s.len());
            res.extend_from_slice(&s[a..end.min(s.len()).max(a)]);
        }
    }
    JsString::from_utf16(res)
}

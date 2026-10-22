//! Flagless ECMAScript regex evaluation over UTF-16 code units.
//!
//! Hook alternatives are split *before* regex recognition, so a regex reaching
//! this module never contains `|`. This evaluator retains JS captures,
//! lookarounds, legacy escapes and character classes without a JS runtime.
//! The matching worklist is iterative (including greedy/lazy repetition).
use super::config::whitespace;
use std::collections::HashMap;

// Empty expansion work = captures + quantifiers, multiplying mandatory nested
// repetitions. Node 22.22.0 succeeds at 2M work across the measured families in
// real HookRuntime.run (+0/32 frames); 40M throws. See probe-resources.mts.
// This is a conservative budget, not an exact model of V8's pattern-dependent
// stack. Ledger: specs/gateway-parity/accepted-diffs.md
// A6.7-HOOK-RESOURCE-001 (unapproved; the old decision was AI-only).
const MAX_EMPTY_REPEAT_WORK: usize = 2_000_000;

#[derive(Debug)]
struct BudgetExceeded;

#[derive(Clone, Debug)]
enum ClassItem {
    Range(u16, u16),
    Digit(bool),
    Word(bool),
    Space(bool),
}
impl ClassItem {
    fn matches(&self, c: u16) -> bool {
        match self {
            Self::Range(a, b) => (*a..=*b).contains(&c),
            Self::Digit(invert) => (48..=57).contains(&c) != *invert,
            Self::Word(invert) => word(c) != *invert,
            Self::Space(invert) => whitespace(c) != *invert,
        }
    }
    fn literal(&self) -> Option<u16> {
        if let Self::Range(a, b) = self
            && a == b
        {
            return Some(*a);
        }
        None
    }
}
#[derive(Debug)]
enum Kind {
    Sequence(Vec<usize>),
    Literal(u16),
    Dot,
    Class(Vec<ClassItem>, bool),
    Start,
    End,
    Boundary(bool),
    Capture(usize, usize),
    Backref(usize),
    NamedRef(Vec<u16>),
    Look {
        body: usize,
        positive: bool,
        behind: bool,
    },
    Repeat {
        body: usize,
        min: usize,
        max: usize,
        lazy: bool,
    },
}
#[derive(Debug)]
struct Node {
    kind: Kind,
    captures: std::ops::Range<usize>,
    zero_width: bool,
}
type GroupHeader = (Option<usize>, Option<(bool, bool)>);
struct Parser<'a> {
    text: &'a [u16],
    pos: usize,
    total: usize,
    count: usize,
    has_names: bool,
    names: HashMap<Vec<u16>, usize>,
    nodes: Vec<Node>,
}
impl<'a> Parser<'a> {
    fn new(text: &'a [u16]) -> Self {
        let (mut total, mut has_names, mut class, mut escaped) = (0, false, false, false);
        for (i, &c) in text.iter().enumerate() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == 92 {
                escaped = true;
                continue;
            }
            if c == 91 && !class {
                class = true;
                continue;
            }
            if c == 93 && class {
                class = false;
                continue;
            }
            if c == 40 && !class {
                if text.get(i + 1) != Some(&63) {
                    total += 1;
                } else if text.get(i + 2) == Some(&60) && !matches!(text.get(i + 3), Some(61 | 33))
                {
                    total += 1;
                    has_names = true;
                }
            }
        }
        Self {
            text,
            pos: 0,
            total,
            count: 0,
            has_names,
            names: HashMap::new(),
            nodes: Vec::new(),
        }
    }
    fn peek(&self) -> Option<u16> {
        self.text.get(self.pos).copied()
    }
    fn take(&mut self, c: u16) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn add(&mut self, kind: Kind, start: usize) -> usize {
        let id = self.nodes.len();
        let zero_width = match &kind {
            Kind::Start | Kind::End | Kind::Boundary(_) | Kind::Look { .. } => true,
            Kind::Sequence(children) => children.iter().all(|id| self.nodes[*id].zero_width),
            Kind::Capture(_, body) => self.nodes[*body].zero_width,
            Kind::Repeat { body, max, .. } => *max == 0 || self.nodes[*body].zero_width,
            _ => false,
        };
        self.nodes.push(Node {
            kind,
            captures: start..self.count + 1,
            zero_width,
        });
        id
    }
    fn parse(&mut self) -> Result<usize, ()> {
        struct Group {
            start: usize,
            children: Vec<usize>,
            capture: Option<usize>,
            look: Option<(bool, bool)>,
        }
        let mut groups = vec![Group {
            start: 1,
            children: Vec::new(),
            capture: None,
            look: None,
        }];
        while let Some(c) = self.peek() {
            let start = self.count + 1;
            if c == 40 {
                self.pos += 1;
                let (capture, look) = self.group_header()?;
                groups.push(Group {
                    start,
                    children: Vec::new(),
                    capture,
                    look,
                });
                continue;
            }
            let body = if c == 41 {
                self.pos += 1;
                if groups.len() == 1 {
                    return Err(());
                }
                let group = groups.pop().unwrap();
                let sequence = self.add(Kind::Sequence(group.children), group.start);
                if let Some((positive, behind)) = group.look {
                    self.add(
                        Kind::Look {
                            body: sequence,
                            positive,
                            behind,
                        },
                        group.start,
                    )
                } else if let Some(index) = group.capture {
                    self.add(Kind::Capture(index, sequence), group.start)
                } else {
                    sequence
                }
            } else {
                self.atom()?
            };
            let body = if let Some((min, max)) = self.quantifier()? {
                match self.nodes[body].kind {
                    Kind::Start
                    | Kind::End
                    | Kind::Boundary(_)
                    | Kind::Look { behind: true, .. } => return Err(()),
                    _ => {}
                }
                let lazy = self.take(63);
                self.add(
                    Kind::Repeat {
                        body,
                        min,
                        max,
                        lazy,
                    },
                    self.nodes[body].captures.start,
                )
            } else {
                body
            };
            groups.last_mut().unwrap().children.push(body);
        }
        if groups.len() != 1 {
            return Err(());
        }
        Ok(self.add(Kind::Sequence(groups.pop().unwrap().children), 1))
    }
    fn group_header(&mut self) -> Result<GroupHeader, ()> {
        if !self.take(63) {
            self.count += 1;
            return Ok((Some(self.count), None));
        }
        if self.take(58) {
            return Ok((None, None));
        }
        if self.take(61) {
            return Ok((None, Some((true, false))));
        }
        if self.take(33) {
            return Ok((None, Some((false, false))));
        }
        if !self.take(60) {
            return Err(());
        }
        if self.take(61) {
            return Ok((None, Some((true, true))));
        }
        if self.take(33) {
            return Ok((None, Some((false, true))));
        }
        let name = self.name()?;
        self.count += 1;
        if self.names.insert(name, self.count).is_some() {
            return Err(());
        }
        Ok((Some(self.count), None))
    }
    fn atom(&mut self) -> Result<usize, ()> {
        let start = self.count + 1;
        let c = self.peek().ok_or(())?;
        self.pos += 1;
        let kind = match c {
            42 | 43 | 63 | 124 => return Err(()),
            123 => {
                self.pos -= 1;
                if self.quantifier()?.is_some() {
                    return Err(());
                }
                self.pos += 1;
                Kind::Literal(123)
            }
            94 => Kind::Start,
            36 => Kind::End,
            46 => Kind::Dot,
            91 => self.class()?,
            92 => self.escape(false)?,
            _ => Kind::Literal(c),
        };
        Ok(self.add(kind, start))
    }
    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        let mut n = 0usize;
        while let Some(c @ 48..=57) = self.peek() {
            // V8 saturates decimal quantifier bounds at INT_MAX.
            n = n
                .saturating_mul(10)
                .saturating_add(usize::from(c - 48))
                .min(i32::MAX as usize);
            self.pos += 1;
        }
        (self.pos != start).then_some(n)
    }
    fn quantifier(&mut self) -> Result<Option<(usize, usize)>, ()> {
        if self.take(42) {
            return Ok(Some((0, usize::MAX)));
        }
        if self.take(43) {
            return Ok(Some((1, usize::MAX)));
        }
        if self.take(63) {
            return Ok(Some((0, 1)));
        }
        let start = self.pos;
        if !self.take(123) {
            return Ok(None);
        }
        if let Some(min) = self.number() {
            let max = if self.take(44) {
                self.number().unwrap_or(usize::MAX)
            } else {
                min
            };
            if self.take(125) {
                if max < min {
                    return Err(());
                }
                return Ok(Some((min, max)));
            }
        }
        self.pos = start;
        Ok(None)
    }
    fn hex(&mut self, count: usize) -> Option<u16> {
        let digits = self.text.get(self.pos..self.pos + count)?;
        let mut n = 0;
        for &c in digits {
            n = n * 16
                + match c {
                    48..=57 => c - 48,
                    65..=70 => c - 55,
                    97..=102 => c - 87,
                    _ => return None,
                };
        }
        self.pos += count;
        Some(n)
    }
    fn name(&mut self) -> Result<Vec<u16>, ()> {
        let mut name = Vec::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == 62 {
                let text = String::from_utf16(&name).map_err(|_| ())?;
                let mut chars = text.chars();
                let valid = chars.next().is_some_and(super::identifier_ranges::is_start)
                    && chars.all(super::identifier_ranges::is_continue);
                return if valid { Ok(name) } else { Err(()) };
            }
            if c == 92 {
                if !self.take(117) {
                    return Err(());
                }
                if self.take(123) {
                    let start = self.pos;
                    while self
                        .peek()
                        .is_some_and(|c| matches!(c, 48..=57 | 65..=70 | 97..=102))
                    {
                        self.pos += 1;
                    }
                    let text = String::from_utf16(&self.text[start..self.pos]).map_err(|_| ())?;
                    let n = u32::from_str_radix(&text, 16).map_err(|_| ())?;
                    let c = char::from_u32(n).ok_or(())?;
                    if !self.take(125) {
                        return Err(());
                    }
                    name.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
                } else {
                    name.push(self.hex(4).ok_or(())?);
                }
            } else {
                name.push(c);
            }
        }
        Err(())
    }
    fn escape(&mut self, class: bool) -> Result<Kind, ()> {
        let c = self.peek().ok_or(())?;
        self.pos += 1;
        let item = match c {
            100 => Some(ClassItem::Digit(false)),
            68 => Some(ClassItem::Digit(true)),
            119 => Some(ClassItem::Word(false)),
            87 => Some(ClassItem::Word(true)),
            115 => Some(ClassItem::Space(false)),
            83 => Some(ClassItem::Space(true)),
            _ => None,
        };
        if let Some(item) = item {
            return Ok(Kind::Class(vec![item], false));
        }
        let literal = match c {
            98 if !class => return Ok(Kind::Boundary(true)),
            66 if !class => return Ok(Kind::Boundary(false)),
            98 => 8,
            102 => 12,
            110 => 10,
            114 => 13,
            116 => 9,
            118 => 11,
            99 => {
                if let Some(next) = self.peek().filter(|c| {
                    matches!(c, 65..=90 | 97..=122) || (class && matches!(c, 48..=57 | 95))
                }) {
                    self.pos += 1;
                    next % 32
                } else {
                    self.pos -= 1;
                    92
                }
            }
            120 => self.hex(2).unwrap_or(120),
            117 => self.hex(4).unwrap_or(117),
            48..=57 => {
                self.pos -= 1;
                let start = self.pos;
                let decimal = self.number().unwrap();
                if !class && c != 48 && decimal <= self.total {
                    return Ok(Kind::Backref(decimal));
                }
                self.pos = start + 1;
                if c <= 55 {
                    let mut n = c - 48;
                    let limit = if c <= 51 { 3 } else { 2 };
                    for _ in 1..limit {
                        if let Some(d @ 48..=55) = self.peek() {
                            n = n * 8 + d - 48;
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    n
                } else {
                    c
                }
            }
            107 if self.has_names => {
                if class || !self.take(60) {
                    return Err(());
                }
                return Ok(Kind::NamedRef(self.name()?));
            }
            _ => c,
        };
        Ok(Kind::Literal(literal))
    }
    fn class_atom(&mut self) -> Result<ClassItem, ()> {
        let c = self.peek().ok_or(())?;
        self.pos += 1;
        if c != 92 {
            return Ok(ClassItem::Range(c, c));
        }
        match self.escape(true)? {
            Kind::Literal(c) => Ok(ClassItem::Range(c, c)),
            Kind::Class(mut items, false) => Ok(items.remove(0)),
            _ => Err(()),
        }
    }
    fn class(&mut self) -> Result<Kind, ()> {
        let negate = self.take(94);
        let mut items = Vec::new();
        while self.peek().is_some() && self.peek() != Some(93) {
            let left = self.class_atom()?;
            if self.peek() == Some(45) && self.text.get(self.pos + 1) != Some(&93) {
                self.pos += 1;
                let right = self.class_atom()?;
                if let (Some(a), Some(b)) = (left.literal(), right.literal()) {
                    if a > b {
                        return Err(());
                    }
                    items.push(ClassItem::Range(a, b));
                } else {
                    items.push(left);
                    items.push(ClassItem::Range(45, 45));
                    items.push(right);
                }
            } else {
                items.push(left);
            }
        }
        if !self.take(93) {
            return Err(());
        }
        Ok(Kind::Class(items, negate))
    }
}
fn word(c: u16) -> bool {
    matches!(c, 65..=90 | 97..=122 | 48..=57 | 95)
}

fn empty_repeat_costs(nodes: &[Node], capture_count: usize) -> Vec<usize> {
    // Backreferences can consume only if their capture has a consuming source.
    // The least fixed point keeps (\1){1e9} and mutually empty references free,
    // just like V8, while retaining costs for references to consuming groups.
    let mut consumes = vec![false; nodes.len()];
    let mut captures = vec![false; capture_count + 1];
    loop {
        let mut changed = false;
        for (id, node) in nodes.iter().enumerate() {
            let can_consume = match &node.kind {
                Kind::Literal(_) | Kind::Dot | Kind::Class(..) => true,
                Kind::Sequence(children) => children.iter().any(|id| consumes[*id]),
                Kind::Capture(_, body) => consumes[*body],
                Kind::Repeat { body, max, .. } => *max > 0 && consumes[*body],
                Kind::Backref(index) => captures[*index],
                _ => false,
            };
            if can_consume && !consumes[id] {
                consumes[id] = true;
                changed = true;
            }
            if let Kind::Capture(index, _) = node.kind {
                captures[index] = consumes[id];
            }
        }
        if !changed {
            break;
        }
    }
    // Parser children precede parents. Even empty captures cost register saves
    // inside a consuming repeat. Assertions retain their capture-save cost;
    // their own dynamic repetitions are checked in their atomic search scope.
    let mut costs = vec![0usize; nodes.len()];
    for (id, node) in nodes.iter().enumerate() {
        costs[id] = match &node.kind {
            Kind::Sequence(children) => children
                .iter()
                .fold(0usize, |sum, id| sum.saturating_add(costs[*id])),
            Kind::Capture(_, body) => costs[*body].saturating_add(1),
            Kind::Repeat { body, min, .. } if consumes[id] => {
                costs[*body].saturating_mul((*min).max(1)).saturating_add(1)
            }
            Kind::Repeat { body, .. } => costs[*body],
            Kind::Look { .. } => node.captures.len(),
            Kind::Backref(_) if consumes[id] => 1,
            _ => 0,
        };
    }
    // Exempt only the entire nonconsuming body, after its captures have been
    // counted in consuming ancestors. This preserves static/self-ref billions.
    for (cost, consumes) in costs.iter_mut().zip(consumes) {
        if !consumes {
            *cost = 0;
        }
    }
    costs
}
#[derive(Clone)]
struct State {
    pos: usize,
    captures: Vec<Option<(usize, usize)>>,
}
#[derive(Clone)]
enum Step {
    Node(usize, bool),
    CaptureEnd(usize, usize),
    Repeat {
        id: usize,
        count: usize,
        reverse: bool,
    },
    RepeatEnd {
        id: usize,
        count: usize,
        reverse: bool,
        start: usize,
        empty_work: usize,
    },
}
#[derive(Clone)]
struct Branch {
    state: State,
    tasks: Vec<Step>,
    empty_work: usize,
}
struct Matcher<'a> {
    nodes: &'a [Node],
    text: &'a [u16],
    empty_costs: &'a [usize],
}
impl Matcher<'_> {
    fn run(&self, initial: Branch) -> Result<Option<State>, BudgetExceeded> {
        // Each assertion owns a search frame; returning discards its remaining
        // choices, implementing atomic lookaround without native recursion.
        struct Search {
            choices: Vec<Branch>,
            caller: Option<(Branch, bool)>,
        }
        let mut searches = vec![Search {
            choices: vec![initial],
            caller: None,
        }];
        'search: loop {
            let Some(mut branch) = searches.last_mut().unwrap().choices.pop() else {
                let search = searches.pop().unwrap();
                match search.caller {
                    None => return Ok(None),
                    Some((caller, false)) => searches.last_mut().unwrap().choices.push(caller),
                    Some((_, true)) => {}
                }
                continue;
            };
            let mut failed = false;
            while let Some(step) = branch.tasks.pop() {
                match step {
                    Step::CaptureEnd(index, start) => {
                        branch.state.captures[index] =
                            Some((start.min(branch.state.pos), start.max(branch.state.pos)));
                    }
                    Step::RepeatEnd {
                        id,
                        count,
                        reverse,
                        start,
                        empty_work,
                    } => {
                        let Kind::Repeat { body, min, .. } = self.nodes[id].kind else {
                            unreachable!()
                        };
                        // Empty iterations beyond the required minimum must fail,
                        // retaining the captures of the preceding iteration.
                        if start == branch.state.pos && count >= min {
                            failed = true;
                            break;
                        }
                        // Only statically zero-width bodies can skip repetitions.
                        // A nullable consuming body may choose empty now and consume
                        // later (e.g. (a*?){2}b\1), changing its final captures.
                        let next_count = if self.nodes[body].zero_width {
                            min
                        } else if start == branch.state.pos {
                            // Internal captures reset on each iteration. At most N
                            // further iterations can consume the N remaining units;
                            // one final empty iteration may overwrite captures. Keep
                            // that entire suffix, compressing only redundant empties.
                            let remaining = if reverse {
                                start
                            } else {
                                self.text.len() - start
                            };
                            (count + 1).max(min.saturating_sub(remaining.saturating_add(1)))
                        } else {
                            count + 1
                        };
                        if start == branch.state.pos {
                            // Replace the current iteration's nested cost with its
                            // full expansion, including all compressed iterations.
                            // Budget state is branch-local and restored on backtrack.
                            let work = self.empty_costs[body]
                                .saturating_mul(next_count.saturating_sub(count));
                            branch.empty_work =
                                branch.empty_work.max(empty_work.saturating_add(work));
                            if branch.empty_work > MAX_EMPTY_REPEAT_WORK {
                                // A resource error must escape negative assertions;
                                // treating it as a miss would incorrectly invert it.
                                return Err(BudgetExceeded);
                            }
                        }
                        branch.tasks.push(Step::Repeat {
                            id,
                            count: next_count,
                            reverse,
                        });
                    }
                    Step::Repeat { id, count, reverse } => {
                        let Kind::Repeat {
                            body,
                            min,
                            max,
                            lazy,
                        } = self.nodes[id].kind
                        else {
                            unreachable!()
                        };
                        if count >= max {
                            continue;
                        }
                        let mut more = branch.clone();
                        for index in self.nodes[body].captures.clone() {
                            more.state.captures[index] = None;
                        }
                        more.tasks.push(Step::RepeatEnd {
                            id,
                            count,
                            reverse,
                            start: more.state.pos,
                            empty_work: more.empty_work,
                        });
                        more.tasks.push(Step::Node(body, reverse));
                        if count < min {
                            branch = more;
                        } else if lazy {
                            searches.last_mut().unwrap().choices.push(more);
                        } else {
                            searches.last_mut().unwrap().choices.push(branch);
                            branch = more;
                        }
                    }
                    Step::Node(id, reverse) => {
                        let pos = branch.state.pos;
                        let next = if reverse {
                            pos.checked_sub(1)
                        } else {
                            (pos < self.text.len()).then_some(pos)
                        };
                        let matched = match &self.nodes[id].kind {
                            Kind::Sequence(children) => {
                                if reverse {
                                    branch
                                        .tasks
                                        .extend(children.iter().map(|id| Step::Node(*id, true)));
                                } else {
                                    branch.tasks.extend(
                                        children.iter().rev().map(|id| Step::Node(*id, false)),
                                    );
                                }
                                true
                            }
                            Kind::Capture(index, body) => {
                                branch.tasks.push(Step::CaptureEnd(*index, pos));
                                branch.tasks.push(Step::Node(*body, reverse));
                                true
                            }
                            Kind::Repeat { .. } => {
                                branch.tasks.push(Step::Repeat {
                                    id,
                                    count: 0,
                                    reverse,
                                });
                                true
                            }
                            Kind::Start => pos == 0,
                            Kind::End => pos == self.text.len(),
                            Kind::Boundary(positive) => {
                                let before = pos.checked_sub(1).is_some_and(|i| word(self.text[i]));
                                let after = self.text.get(pos).is_some_and(|c| word(*c));
                                (before != after) == *positive
                            }
                            Kind::Backref(index) => {
                                if let Some((start, end)) = branch.state.captures[*index] {
                                    let len = end - start;
                                    let target = if reverse {
                                        pos.checked_sub(len).map(|p| (p, pos))
                                    } else {
                                        pos.checked_add(len)
                                            .filter(|e| *e <= self.text.len())
                                            .map(|e| (pos, e))
                                    };
                                    if let Some((a, b)) = target
                                        .filter(|(a, b)| self.text[*a..*b] == self.text[start..end])
                                    {
                                        branch.state.pos = if reverse { a } else { b };
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    true
                                }
                            }
                            Kind::Look {
                                body,
                                positive,
                                behind,
                            } => {
                                let child = Branch {
                                    state: branch.state.clone(),
                                    tasks: vec![Step::Node(*body, *behind)],
                                    empty_work: branch.empty_work,
                                };
                                searches.push(Search {
                                    choices: vec![child],
                                    caller: Some((branch, *positive)),
                                });
                                continue 'search;
                            }
                            Kind::NamedRef(_) => unreachable!("names resolved at compile time"),
                            kind => {
                                if let Some(index) = next.filter(|i| {
                                    let c = self.text[*i];
                                    match kind {
                                        Kind::Literal(literal) => c == *literal,
                                        Kind::Dot => !matches!(c, 10 | 13 | 0x2028 | 0x2029),
                                        Kind::Class(items, negate) => {
                                            items.iter().any(|item| item.matches(c)) != *negate
                                        }
                                        _ => unreachable!(),
                                    }
                                }) {
                                    branch.state.pos = if reverse { index } else { index + 1 };
                                    true
                                } else {
                                    false
                                }
                            }
                        };
                        if !matched {
                            failed = true;
                            break;
                        }
                    }
                }
            }
            if !failed {
                let search = searches.pop().unwrap();
                match search.caller {
                    None => return Ok(Some(branch.state)),
                    Some((mut caller, true)) => {
                        caller.state.captures = branch.state.captures;
                        searches.last_mut().unwrap().choices.push(caller);
                    }
                    Some((_, false)) => {}
                }
            }
        }
    }
}
pub(super) fn test(pattern: &[u16], query: &[u16]) -> bool {
    let mut parser = Parser::new(pattern);
    let Ok(root) = parser.parse() else {
        return false;
    };
    if parser.pos != pattern.len() {
        return false;
    }
    for node in &mut parser.nodes {
        if let Kind::NamedRef(name) = &node.kind {
            let Some(index) = parser.names.get(name) else {
                return false;
            };
            node.kind = Kind::Backref(*index);
        }
    }
    let empty_costs = empty_repeat_costs(&parser.nodes, parser.count);
    let matcher = Matcher {
        nodes: &parser.nodes,
        text: query,
        empty_costs: &empty_costs,
    };
    for pos in 0..=query.len() {
        match matcher.run(Branch {
            state: State {
                pos,
                captures: vec![None; parser.count + 1],
            },
            tasks: vec![Step::Node(root, false)],
            empty_work: 0,
        }) {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(BudgetExceeded) => return false,
        }
    }
    false
}

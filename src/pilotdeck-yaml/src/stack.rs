//! Runtime adapter: keep the upstream recursive composition/conversion functions,
//! but do not depend on a caller's (often 2 MiB) native thread stack.
//! Depth limits below are the lower bounds measured with Node v22.22.0,
//! yaml 2.9.0, default V8 stack, five fresh-process binary searches per shape.
//! Source: tests/fixtures/depth.json; reproduce with tests/record_depth.mjs.
//! Native worker stack is only an adapter for bounded recursion, never a limit.
use crate::parse::cst::{Token, TokenType};

pub(crate) const EXHAUSTION_MESSAGE: &str = "Maximum call stack size exceeded";
pub(crate) const CONVERSION_DEPTH: usize = 782;

#[derive(Clone, Copy, Debug)]
pub(crate) struct DepthLimit {
    pub first_exhausted: usize,
    pub catch_depth: usize,
}
pub(crate) const FLOW_SEQ: DepthLimit = DepthLimit {
    first_exhausted: 664,
    catch_depth: 663,
};
pub(crate) const FLOW_MAP: DepthLimit = DepthLimit {
    first_exhausted: 664,
    catch_depth: 663,
};
pub(crate) const BLOCK_SEQ: DepthLimit = DepthLimit {
    first_exhausted: 782,
    catch_depth: 782,
};
pub(crate) const BLOCK_MAP: DepthLimit = DepthLimit {
    first_exhausted: 738,
    catch_depth: 738,
};
pub(crate) const MIXED: DepthLimit = DepthLimit {
    first_exhausted: 664,
    catch_depth: 664,
};

pub(crate) fn compose_limit(root: &Token) -> DepthLimit {
    fn shape(t: &Token) -> TokenType {
        if t.kind == TokenType::FlowCollection {
            t.flow_start.as_ref().unwrap().kind
        } else {
            t.kind
        }
    }
    let mut pending = vec![root];
    let mut first = None;
    while let Some(t) = pending.pop() {
        if t.is_collection() {
            let kind = shape(t);
            if first.is_some_and(|prev| prev != kind) {
                return MIXED;
            }
            first = Some(kind);
        }
        if let Some(value) = t.value.as_deref() {
            pending.push(value);
        }
        for item in &t.items {
            if let Some(Some(key)) = &item.key {
                pending.push(key);
            }
            if let Some(value) = item.value.as_deref() {
                pending.push(value);
            }
        }
    }
    match first {
        Some(TokenType::BlockSeq) => BLOCK_SEQ,
        Some(TokenType::BlockMap) => BLOCK_MAP,
        Some(TokenType::FlowMapStart) => FLOW_MAP,
        _ => FLOW_SEQ,
    }
}

pub(crate) fn conversion_exhausted() -> crate::value::ConversionError {
    crate::value::ConversionError::new("RangeError", EXHAUSTION_MESSAGE)
}
pub(crate) fn with_large_stack<T: Send>(needed: bool, f: impl FnOnce() -> T + Send) -> T {
    if !needed {
        return f();
    }
    std::thread::scope(|scope| {
        match std::thread::Builder::new()
            .name("pilotdeck-yaml".into())
            .stack_size(32 * 1024 * 1024)
            .spawn_scoped(scope, f)
            .expect("Unable to allocate YAML parsing stack")
            .join()
        {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

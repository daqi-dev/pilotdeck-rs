//! One retained wire-byte budget across input, application/cache and required
//! output. This bounds retained payloads, not allocator overhead or user handlers.
use super::{HostError, HostErrorKind};
use pilotdeck_protocol::{JsValue, stringify_json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
pub(super) struct Budget {
    maximum: usize,
    used: AtomicUsize,
}
impl Budget {
    pub(super) fn new(maximum: usize) -> Arc<Self> {
        Arc::new(Self {
            maximum,
            used: AtomicUsize::new(0),
        })
    }
    pub(super) fn reserve(self: &Arc<Self>, bytes: usize) -> Result<Lease, HostError> {
        let mut used = self.used.load(Ordering::SeqCst);
        loop {
            let next = used
                .checked_add(bytes)
                .filter(|total| *total <= self.maximum)
                .ok_or_else(|| {
                    HostError::new(
                        HostErrorKind::Limit,
                        "Host retained wire-byte budget exceeded.",
                    )
                })?;
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(current) => used = current,
            }
        }
        Ok(Lease {
            budget: self.clone(),
            bytes,
        })
    }
    pub(super) fn retain(self: &Arc<Self>, value: JsValue) -> Result<Frame, HostError> {
        let bytes = stringify_json(&value)
            .map_err(|e| HostError::protocol(e.to_string()))?
            .ok_or_else(|| HostError::protocol("Cannot retain undefined."))?
            .len()
            + 1;
        Ok(Frame {
            value,
            lease: self.reserve(bytes)?,
        })
    }
}
#[derive(Debug)]
pub(super) struct Lease {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Lease {
    pub(super) fn grow(&mut self, bytes: usize) -> Result<(), HostError> {
        let mut extra = self.budget.reserve(bytes)?;
        self.bytes += bytes;
        extra.bytes = 0;
        Ok(())
    }
    pub(super) fn duplicate(&self) -> Result<Self, HostError> {
        self.budget.reserve(self.bytes)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}
#[derive(Debug)]
pub(super) struct Frame {
    pub(super) value: JsValue,
    pub(super) lease: Lease,
}
impl std::ops::Deref for Frame {
    type Target = JsValue;
    fn deref(&self) -> &JsValue {
        &self.value
    }
}

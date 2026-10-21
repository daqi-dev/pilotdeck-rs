//! Volatile, session- and provider-generation-scoped circuit observations.
//! Mirrors `router/health/{ProviderHealthTracker,RouterProviderHealthPort}.ts`.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Open,
    HalfOpen,
}
impl HealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

#[derive(Clone, Debug)]
pub struct HealthOptions {
    pub degrade_threshold: f64,
    pub open_threshold: f64,
    pub open_duration_ms: f64,
    pub window_size: f64,
}
impl Default for HealthOptions {
    fn default() -> Self {
        Self {
            degrade_threshold: 3.0,
            open_threshold: 5.0,
            open_duration_ms: 30_000.0,
            window_size: 20.0,
        }
    }
}
struct Record {
    state: HealthState,
    failures: f64,
    opened_at: f64,
    window: Vec<bool>,
}
impl Default for Record {
    fn default() -> Self {
        Self {
            state: HealthState::Healthy,
            failures: 0.0,
            opened_at: 0.0,
            window: Vec::new(),
        }
    }
}

pub struct ProviderHealthTracker {
    options: HealthOptions,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    // Map insertion order is observable in snapshot().
    records: Vec<(JsString, Record)>,
}
impl ProviderHealthTracker {
    pub fn new(options: HealthOptions, clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            options,
            clock,
            records: Vec::new(),
        }
    }
    fn record(&mut self, provider: &JsString) -> &mut Record {
        let index = self
            .records
            .iter()
            .position(|(key, _)| key == provider)
            .unwrap_or_else(|| {
                self.records.push((provider.clone(), Record::default()));
                self.records.len() - 1
            });
        &mut self.records[index].1
    }
    pub fn record_success(&mut self, provider: &JsString) {
        let size = self.options.window_size;
        let record = self.record(provider);
        record.failures = 0.0;
        record.window.push(true);
        if record.window.len() as f64 > size {
            record.window.remove(0);
        }
        record.state = HealthState::Healthy;
    }
    pub fn record_failure(&mut self, provider: &JsString) {
        let options = self.options.clone();
        let clock = self.clock.clone();
        let record = self.record(provider);
        record.failures += 1.0;
        record.window.push(false);
        if record.window.len() as f64 > options.window_size {
            record.window.remove(0);
        }
        if record.failures >= options.open_threshold {
            if record.state != HealthState::Open {
                record.state = HealthState::Open;
                record.opened_at = clock();
            }
        } else if record.failures >= options.degrade_threshold
            && record.state == HealthState::Healthy
        {
            record.state = HealthState::Degraded;
        }
        if record.state == HealthState::HalfOpen {
            record.state = HealthState::Open;
            record.opened_at = clock();
        }
    }
    pub fn get_state(&mut self, provider: &JsString) -> HealthState {
        let Some((_, record)) = self.records.iter_mut().find(|(key, _)| key == provider) else {
            return HealthState::Healthy;
        };
        if record.state == HealthState::Open
            && (self.clock)() - record.opened_at >= self.options.open_duration_ms
        {
            record.state = HealthState::HalfOpen;
        }
        record.state
    }
    pub fn should_skip(&mut self, provider: &JsString) -> bool {
        self.get_state(provider) == HealthState::Open
    }
    pub fn is_available(&mut self, provider: &JsString) -> bool {
        !self.should_skip(provider)
    }
    pub fn success_rate(&self, provider: &JsString) -> f64 {
        self.records
            .iter()
            .find(|(key, _)| key == provider)
            .map_or(1.0, |(_, record)| {
                if record.window.is_empty() {
                    1.0
                } else {
                    record.window.iter().filter(|success| **success).count() as f64
                        / record.window.len() as f64
                }
            })
    }
    pub fn reset(&mut self, provider: &JsString) {
        self.records.retain(|(key, _)| key != provider);
    }
    pub fn reset_all(&mut self) {
        self.records.clear();
    }
    /// Ordered entries, like `[...tracker.snapshot()]` (numeric provider ids
    /// must not be reordered by JS object enumeration).
    pub fn snapshot(&mut self) -> JsValue {
        let keys: Vec<_> = self.records.iter().map(|(key, _)| key.clone()).collect();
        JsValue::Array(
            keys.iter()
                .map(|key| {
                    let mut value = JsObject::new();
                    value.insert("state".into(), self.get_state(key).as_str().into());
                    value.insert(
                        "successRate".into(),
                        JsValue::Number(self.success_rate(key)),
                    );
                    let failures = self
                        .records
                        .iter()
                        .find(|(id, _)| id == key)
                        .unwrap()
                        .1
                        .failures;
                    value.insert("consecutiveFailures".into(), JsValue::Number(failures));
                    JsValue::Array(vec![JsValue::String(key.clone()), JsValue::Object(value)])
                })
                .collect(),
        )
    }
}

#[derive(Clone, Debug)]
pub struct ProviderHealthInput {
    pub session_id: JsString,
    pub provider_id: JsString,
    /// String/number generations from the invocation port; nullish is legacy.
    pub provider_generation: JsValue,
}
impl ProviderHealthInput {
    fn key(&self) -> JsString {
        let generation = match &self.provider_generation {
            JsValue::String(value) => value.clone(),
            JsValue::Number(value) => crate::js_value::number_string(*value).into(),
            _ => "legacy".into(),
        };
        let mut units = self.provider_id.units().to_vec();
        units.push(0);
        units.extend_from_slice(generation.units());
        JsString::from_utf16(units)
    }
}
pub trait RouterProviderHealth: Send + Sync {
    fn should_skip(&self, input: &ProviderHealthInput) -> bool;
    fn record_failure(&self, input: &ProviderHealthInput);
    fn record_success(&self, input: &ProviderHealthInput);
    fn dispose(&self) {}
}
pub struct NativeRouterProviderHealth {
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    trackers: Mutex<Vec<(JsString, ProviderHealthTracker)>>,
}
impl NativeRouterProviderHealth {
    pub fn new(clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            clock,
            trackers: Mutex::new(Vec::new()),
        }
    }
    fn with_tracker<T>(
        &self,
        input: &ProviderHealthInput,
        f: impl FnOnce(&mut ProviderHealthTracker, &JsString) -> T,
    ) -> T {
        let mut trackers = self.trackers.lock().expect("provider health");
        let index = trackers
            .iter()
            .position(|(key, _)| key == &input.session_id)
            .unwrap_or_else(|| {
                trackers.push((
                    input.session_id.clone(),
                    ProviderHealthTracker::new(HealthOptions::default(), self.clock.clone()),
                ));
                trackers.len() - 1
            });
        f(&mut trackers[index].1, &input.key())
    }
}
impl RouterProviderHealth for NativeRouterProviderHealth {
    fn should_skip(&self, input: &ProviderHealthInput) -> bool {
        self.with_tracker(input, |tracker, key| tracker.should_skip(key))
    }
    fn record_failure(&self, input: &ProviderHealthInput) {
        self.with_tracker(input, |tracker, key| tracker.record_failure(key));
    }
    fn record_success(&self, input: &ProviderHealthInput) {
        self.with_tracker(input, |tracker, key| tracker.record_success(key));
    }
    fn dispose(&self) {
        self.trackers.lock().expect("provider health").clear();
    }
}

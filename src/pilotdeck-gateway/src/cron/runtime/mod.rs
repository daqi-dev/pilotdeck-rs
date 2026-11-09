//! Native automation execution. The application owns the serial agent host.
mod background;
mod fire;
mod host;
mod scheduler;
pub use super::always_on::{AlwaysOnError as RuntimeError, EventSink, HostFuture};
pub use background::BackgroundService;
pub use host::{NativeAlwaysOnAdapter, NativeHost, NativeTurnRequest};
pub use scheduler::CronRuntime;
#[cfg(test)]
mod tests;

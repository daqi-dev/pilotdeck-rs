//! Port of the six cron control adapters (InProcessGateway.ts:2603-2625):
//! `GatewayCronController = CronControlPort`, so each adapter is one
//! delegation to the corresponding `CronManager` method. Every
//! parameter-reading and validation behavior the TS method bodies own —
//! the `typeof`/`?.trim()`/`Number.isSafeInteger` checks and their
//! `TypeError`/`Error` shapes (CronRuntime.ts:220-403, CronManager.ts:115-168,
//! 273-278) — lives in [`super::manager`] with the state it validates
//! against; this file is the stable call surface the wire projection
//! (`projection.rs`, #1326's dispatch) and the gold replay drive, so neither
//! has to reach into the manager's internals.
//!
//! `frame.params` crosses here exactly as the WS layer passes it
//! (GatewayWsConnection.ts:464-475: `frame.params as never`): an absent
//! `params` is `undefined`, and the adapter's property reads then throw
//! V8's `Cannot read properties of undefined (reading '<field>')` — the one
//! exception is `cron_list`, whose `input = {}` default (CronRuntime.ts:281)
//! swallows `undefined` before the first read.
use pilotdeck_protocol::JsValue;

use super::{CronError, CronManager};

/// `cronCreate` (InProcessGateway.ts:2603-2605) over `requireCron().createTask`.
pub fn cron_create(manager: &CronManager, params: &JsValue) -> Result<JsValue, CronError> {
    manager.create_task(params)
}

/// `cronList` (InProcessGateway.ts:2607-2609) over `requireCron().listTasks`.
pub fn cron_list(manager: &CronManager, params: &JsValue) -> Result<JsValue, CronError> {
    manager.list_tasks(params)
}

/// `cronUpdate` (InProcessGateway.ts:2611-2613) over `requireCron().updateTask`.
pub fn cron_update(manager: &CronManager, params: &JsValue) -> Result<JsValue, CronError> {
    manager.update_task(params)
}

/// `cronDelete` (InProcessGateway.ts:2615-2617) over `requireCron().deleteTask`.
pub fn cron_delete(manager: &CronManager, params: &JsValue) -> Result<JsValue, CronError> {
    manager.delete_task(params)
}

/// `cronStop` (InProcessGateway.ts:2619-2621) over `requireCron().stopTask`.
pub fn cron_stop(manager: &CronManager, params: &JsValue) -> Result<JsValue, CronError> {
    manager.stop_task(params)
}

/// `cronRunNow` (InProcessGateway.ts:2623-2625) over `requireCron().runTaskNow`.
pub fn cron_run_now(manager: &CronManager, params: &JsValue) -> Result<JsValue, CronError> {
    manager.run_task_now(params)
}

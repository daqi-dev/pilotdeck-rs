//! Foreground activity observed after authentication, shared by native and paired turns.
use super::always_on::AlwaysOnService;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
#[derive(Clone)]
struct Turn {
    project: String,
    channel: String,
    writer: String,
}
#[derive(Default)]
pub struct Activity {
    service: Mutex<Weak<AlwaysOnService>>,
    turns: Mutex<HashMap<(String, String), Turn>>,
}
impl Activity {
    pub fn bind(&self, service: &Arc<AlwaysOnService>) {
        *self.service.lock().unwrap() = Arc::downgrade(service);
    }
    pub fn begin(&self, writer: &str, id: &str, params: &Value) {
        let (Some(project), Some(session)) =
            (params["projectKey"].as_str(), params["sessionKey"].as_str())
        else {
            return;
        };
        if session.starts_with("cron:") || session.starts_with("always-on/") {
            return;
        }
        let channel = params["channelKey"].as_str().unwrap_or("web");
        let turn = Turn {
            project: project.into(),
            channel: channel.into(),
            writer: writer.into(),
        };
        let mut turns = self.turns.lock().unwrap();
        turns.insert((writer.into(), id.into()), turn);
        if let Some(service) = self.service.lock().unwrap().upgrade() {
            let _=service.set_lease(json!({"projectKey":project,"channelKey":channel,"writerId":writer,"sessionKey":session,"agentBusy":true}));
            service.mark_busy(project, channel, writer);
            service.set_project_busy(project, true);
        }
    }
    pub fn end(&self, writer: &str, id: &str) {
        let mut turns = self.turns.lock().unwrap();
        let Some(turn) = turns.remove(&(writer.into(), id.into())) else {
            return;
        };
        if let Some(service) = self.service.lock().unwrap().upgrade() {
            let busy = turns.values().any(|t| t.project == turn.project);
            let channel_busy = turns.values().any(|t| {
                t.project == turn.project && t.channel == turn.channel && t.writer == turn.writer
            });
            if !channel_busy {
                service.mark_idle(&turn.project, &turn.channel, &turn.writer);
            }
            service.set_project_busy(&turn.project, busy);
        }
    }
    pub fn disconnect(&self, writer: &str) {
        let mut turns = self.turns.lock().unwrap();
        let projects = turns
            .values()
            .filter(|t| t.writer == writer)
            .map(|t| t.project.clone())
            .collect::<Vec<_>>();
        turns.retain(|(w, _), _| w != writer);
        if let Some(service) = self.service.lock().unwrap().upgrade() {
            service.remove_writer(writer);
            for project in projects {
                service.set_project_busy(&project, turns.values().any(|t| t.project == project));
            }
        }
    }
    pub fn observe(&self, writer: &str, frame: &Value) {
        if (frame["type"] == "event" && frame["final"] == true
            || frame["type"] == "response" && frame["ok"] == false)
            && let Some(id) = frame["id"].as_str()
        {
            self.end(writer, id);
        }
    }
    pub fn guard(self: &Arc<Self>) -> ConnectionActivity {
        ConnectionActivity {
            activity: self.clone(),
            writer: uuid::Uuid::new_v4().to_string(),
        }
    }
}
pub struct ConnectionActivity {
    pub activity: Arc<Activity>,
    pub writer: String,
}
impl Drop for ConnectionActivity {
    fn drop(&mut self) {
        self.activity.disconnect(&self.writer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::always_on::HostFuture;
    use crate::cron::always_on::{AlwaysOnHost, AlwaysOnService, TurnRequest};
    struct Unused;
    impl AlwaysOnHost for Unused {
        fn turn(&self, _: TurnRequest) -> HostFuture<'_, Vec<Value>> {
            Box::pin(async { panic!("gates must prevent native invocation") })
        }
        fn abort<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str) -> HostFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self, _: &'a str) -> HostFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn on_turn_event(&self, _: &str, _: &str, _: &Value) {}
    }
    #[tokio::test]
    async fn busy_refcounts_terminal_and_disconnect_reach_actual_discovery_gate() {
        let root = std::env::temp_dir().join(format!("pdrs-activity-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project = project.to_string_lossy().to_string();
        let service = Arc::new(AlwaysOnService::new(
            root.join("home"),
            json!({"projects":{&project:{"enabled":true}}}),
            Arc::new(Unused),
        ));
        let activity = Arc::new(Activity::default());
        activity.bind(&service);
        let writer = activity.guard();
        let second = activity.guard();
        activity.begin(
            &writer.writer,
            "a",
            &json!({"projectKey":project,"sessionKey":"web:a","channelKey":"web"}),
        );
        activity.begin(
            &second.writer,
            "b",
            &json!({"projectKey":project,"sessionKey":"web:b","channelKey":"web"}),
        );
        assert_eq!(
            service.run_tick_once(&project).await.unwrap()["reason"],
            "agent_busy"
        );
        activity.observe(
            &writer.writer,
            &json!({"type":"event","id":"a","final":true}),
        );
        assert_eq!(
            service.run_tick_once(&project).await.unwrap()["reason"],
            "agent_busy"
        );
        drop(second);
        assert_eq!(service.leases().len(), 1);
        assert_eq!(
            service.run_tick_once(&project).await.unwrap()["reason"],
            "recent_user_msg"
        );
        activity.begin(
            &writer.writer,
            "c",
            &json!({"projectKey":project,"sessionKey":"cron:owned","channelKey":"cron"}),
        );
        assert_eq!(service.leases().len(), 1);
        drop(writer);
        assert!(service.leases().is_empty());
        service.stop().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}

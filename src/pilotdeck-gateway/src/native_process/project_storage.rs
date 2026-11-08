//! A native session's transcript is opened from the same immutable project
//! generation that prepared its model, tools and permissions. The factory's
//! generic default create-storage path is unchanged.
use crate::runtime::{
    project::{
        ProjectResources, Resource, RuntimeError, RuntimeLease, persistence::PersistenceResources,
    },
    session::factory::{CreateStorage, RecordingPlan, SessionStorage},
};
use std::{path::PathBuf, sync::Arc};

pub(super) fn for_generation(home: PathBuf, generation: Arc<RuntimeLease>) -> CreateStorage {
    Arc::new(move |context| {
        let generation = generation.clone();
        let home = home.clone();
        Box::pin(async move {
            let project = generation.runtime().project_root().to_path_buf();
            let resource = generation
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| RuntimeError::new("Native persistence project is missing."))?
                .context()
                .resource(Resource::Persistence)
                .ok_or_else(|| RuntimeError::new("Native persistence is missing."))?;
            let (chat_dir, permit) =
                super::methods::session_dir::resolve_chat_dir(home.clone(), project.clone())
                    .await
                    .map_err(|error| {
                        RuntimeError::new(String::from_utf16_lossy(error.message.units()))
                    })?;
            let path = chat_dir.join(format!(
                "{}.jsonl",
                crate::storage::sanitize_session_id(&context.session_key)
            ));
            let open_path = path.clone();
            let permit = Arc::new(permit);
            Ok(SessionStorage::Recording(RecordingPlan {
                transcript_path: path,
                open: Arc::new(move || {
                    let _permit = &permit;
                    let _generation = &generation;
                    let current = super::methods::session_dir::bounded_chat_dir(&home, &project)
                        .map_err(|error| {
                            crate::storage::recorder::RecorderError::Type(String::from_utf16_lossy(
                                error.message.units(),
                            ))
                        })?;
                    if current != chat_dir {
                        return Err(crate::storage::recorder::RecorderError::Type(
                            "Native session project marker changed while opening.".into(),
                        ));
                    }
                    let persistence = resource
                        .as_any()
                        .downcast_ref::<PersistenceResources>()
                        .ok_or_else(|| {
                            crate::storage::recorder::RecorderError::Type(
                                "Native persistence has the wrong type.".into(),
                            )
                        })?;
                    persistence.create_session_bounded(
                        context.session_key.clone(),
                        Some(open_path.clone()),
                    )
                }),
            }))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::session::SessionContext;
    #[tokio::test]
    async fn native_storage_uses_prepared_generation_root_instead_of_fallback_or_input() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let a = requests.options.project.clone();
        let b = a.parent().unwrap().join("storage-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut selected = requests.options.clone();
        selected.project = b.clone();
        super::super::assembly::register_project(&selected).unwrap();
        let registry = super::super::assembly::project_registry(&requests.options, &requests.env);
        let generation = Arc::new(registry.acquire(Some(&b)).unwrap());
        let factory = for_generation(requests.options.home.clone(), generation.clone());
        let result = factory(SessionContext {
            session_key: "same".into(),
            // Internal regression stimulus: storage must follow the prepared
            // generation, rather than resolving another lease from this value.
            project_key: Some(a.to_string_lossy().into_owned().into()),
            channel_key: "cli".into(),
            allowed_tools: None,
            disallowed_tools: None,
        })
        .await
        .unwrap();
        let SessionStorage::Recording(plan) = result else {
            panic!("native recording plan");
        };
        let b_dir =
            super::super::methods::session_dir::bounded_chat_dir(&requests.options.home, &b)
                .unwrap();
        let a_dir =
            super::super::methods::session_dir::bounded_chat_dir(&requests.options.home, &a)
                .unwrap();
        assert_eq!(plan.transcript_path, b_dir.join("same.jsonl"));
        assert_ne!(plan.transcript_path, a_dir.join("same.jsonl"));
        let opened = (plan.open)().unwrap();
        assert!(opened.is_persistent());
        // Opening an empty recording is lazy; its first append creates JSONL.
        opened
            .recorder
            .start_turn(&"same".into(), &"owned-B".into())
            .unwrap();
        assert_eq!(opened.read().unwrap().entries.len(), 1);
        assert!(plan.transcript_path.exists());
        assert!(!a_dir.join("same.jsonl").exists());
        drop(opened);
        drop(plan);
        drop(factory);
        drop(generation);
        tokio::time::timeout(std::time::Duration::from_secs(8), registry.dispose())
            .await
            .unwrap()
            .unwrap();
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}

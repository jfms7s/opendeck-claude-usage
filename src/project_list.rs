//! The plugin → Property Inspector project list behind the Metric Tile and
//! Heatmap "Scope" select. The page asks with `sendToPlugin
//! {"request":"projects"}` once connected, and the plugin also sends the
//! list on `propertyInspectorDidAppear` (whichever arrives after the page
//! listens fills it).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::source::logs::{LogUsageSource, ProjectInfo};

/// The most projects the list carries (the most recently used ones).
pub const MAX_PROJECTS: usize = 50;

/// Whether a `sendToPlugin` payload asks for the project list.
pub fn is_projects_request(payload: &Value) -> bool {
    payload.get("request").and_then(Value::as_str) == Some("projects")
}

/// `{"projects":[{"id","name"}, ...]}`, at most `MAX_PROJECTS`.
pub fn projects_message(projects: &[ProjectInfo]) -> Value {
    let kept = &projects[..projects.len().min(MAX_PROJECTS)];
    json!({ "projects": kept })
}

/// Sends the list to `instance_id`'s Property Inspector from a task of its
/// own: listing waits for a running transcript scan, and openaction
/// handles events one at a time.
pub fn send_projects(logs: Arc<LogUsageSource>, instance_id: String) {
    tokio::spawn(async move {
        let message = projects_message(&logs.projects().await);
        if let Some(instance) = openaction::get_instance(instance_id).await
            && let Err(e) = instance.send_to_property_inspector(message).await
        {
            log::warn!("could not send the project list: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(i: usize) -> ProjectInfo {
        ProjectInfo {
            id: format!("-home-u-p{i}"),
            name: format!("p{i}"),
        }
    }

    #[test]
    fn only_a_projects_request_is_one() {
        assert!(is_projects_request(&json!({"request": "projects"})));
        for other in [
            json!({}),
            json!({"request": "x"}),
            json!("projects"),
            json!(null),
        ] {
            assert!(!is_projects_request(&other), "{other}");
        }
    }

    #[test]
    fn the_message_lists_ids_and_names_capped() {
        let list: Vec<ProjectInfo> = (0..MAX_PROJECTS + 5).map(project).collect();
        let message = projects_message(&list);
        let sent = message["projects"].as_array().unwrap();
        assert_eq!(sent.len(), MAX_PROJECTS);
        assert_eq!(sent[0], json!({"id": "-home-u-p0", "name": "p0"}));
    }
}

use super::*;
use crate::app_server::AppServer;
use focus_addon_sdk::AssistantSession;
use std::{sync::mpsc::Receiver, time::Duration};

fn ready() -> (CodexUi, Receiver<Value>) {
    let (server, requests) = AppServer::capture_requests();
    let mut ui = CodexUi::default();
    ui.server = Some(server);
    ui.thread_id = Some("old-session".into());
    ui.model = "model".into();
    ui.models = vec![
        json!({"model":"model","displayName":"Model","defaultReasoningEffort":"low",
        "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}),
    ];
    ui.modes = vec![
        json!({"mode":"default","name":"Default"}),
        json!({"mode":"plan","name":"Plan"}),
    ];
    ui.input = "unsent draft".into();
    (ui, requests)
}
#[test]
fn issue_opens_a_fresh_thread_then_submits_once_with_plan_and_high() {
    let (mut ui, requests) = ready();
    let selection = ui.assistant_capabilities().preferred();
    ui.start_issue(
        AssistantSession {
            selection,
            prompt: "Issue #42\nFix startup".into(),
        },
        None,
    )
    .unwrap();
    let create = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(create["method"], "thread/start");
    assert!(
        !create["params"]
            .as_object()
            .unwrap()
            .contains_key("threadId")
    );
    assert!(requests.try_recv().is_err());
    let result =
        json!({"result":{"thread":{"id":"issue-session"},"model":"model","reasoningEffort":"low"}});
    ui.handle_response(&create["id"], &result, None);
    let prompt = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(prompt["method"], "turn/start");
    assert_eq!(prompt["params"]["threadId"], "issue-session");
    assert_eq!(prompt["params"]["effort"], "high");
    assert_eq!(prompt["params"]["collaborationMode"]["mode"], "plan");
    assert_eq!(
        prompt["params"]["input"][0]["text"],
        "Issue #42\nFix startup"
    );
    assert_eq!(ui.input, "unsent draft");
    ui.handle_response(&create["id"], &result, None);
    assert!(requests.try_recv().is_err());
}
#[test]
fn failed_thread_creation_does_not_submit_issue_to_previous_session() {
    let (mut ui, requests) = ready();
    let selection = ui.assistant_capabilities().preferred();
    ui.start_issue(
        AssistantSession {
            selection,
            prompt: "Issue".into(),
        },
        None,
    )
    .unwrap();
    let create = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    ui.handle_response(
        &create["id"],
        &json!({"error":{"message":"cannot create"}}),
        None,
    );
    assert!(ui.issue_launch.is_none());
    assert!(!ui.busy);
    assert_eq!(ui.thread_id.as_deref(), Some("old-session"));
    assert!(requests.try_recv().is_err());
}

#[test]
fn context_change_cancels_issue_before_draining_old_thread_response() {
    let (mut ui, requests) = ready();
    let selection = ui.assistant_capabilities().preferred();
    ui.start_issue(
        AssistantSession {
            selection,
            prompt: "Old workspace issue".into(),
        },
        None,
    )
    .unwrap();
    let create = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    ui.cancel_issue_start();
    ui.handle_response(
        &create["id"],
        &json!({"result":{"thread":{"id":"old-workspace-thread"},"model":"model"}}),
        None,
    );
    assert!(requests.try_recv().is_err());
}

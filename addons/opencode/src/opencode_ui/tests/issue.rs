use super::*;
use focus_addon_sdk::AssistantSession;

fn prepare(ui: &mut OpenCodeUi) {
    ui.input = "unsent draft".into();
    ui.load_models(json!({"connected":["provider"],"default":{"provider":"model"},"all":[{
        "id":"provider", "name":"Provider", "models":{"model":{"name":"Model","variants":{"low":{},"high":{}}}}
    }]}));
}
#[test]
fn fresh_issue_session_uses_selected_agent_model_and_variant_without_losing_draft() {
    let (mut ui, requests, _) = connected();
    prepare(&mut ui);
    let selection = ui.assistant_capabilities().preferred();
    ui.start_issue(AssistantSession {
        selection,
        prompt: "Issue #42\nFix startup".into(),
    })
    .unwrap();
    let create = requests.recv().unwrap();
    assert_eq!(create.method, "POST");
    assert_eq!(create.path, "/session");
    assert!(requests.try_recv().is_err());
    ui.reply(create.id, Ok(json!({"id":"issue-session"})));
    while let Ok(request) = requests.try_recv() {
        let value = if request.path.ends_with("/message?limit=100") {
            json!([])
        } else {
            json!({})
        };
        ui.reply(request.id, Ok(value));
    }
    ui.progress_issue();
    let prompt = requests.recv().unwrap();
    assert_eq!(prompt.path, "/session/issue-session/prompt_async");
    let body = prompt.body.unwrap();
    assert_eq!(body["agent"], "plan");
    assert_eq!(
        body["model"],
        json!({"providerID":"provider","modelID":"model"})
    );
    assert_eq!(body["variant"], "high");
    assert_eq!(body["parts"][0]["text"], "Issue #42\nFix startup");
    assert_eq!(ui.input, "unsent draft");
    ui.progress_issue();
    assert!(requests.try_recv().is_err());
}
#[test]
fn failed_session_creation_never_sends_prompt() {
    let (mut ui, requests, _) = connected();
    prepare(&mut ui);
    let selection = ui.assistant_capabilities().preferred();
    ui.start_issue(AssistantSession {
        selection,
        prompt: "Issue".into(),
    })
    .unwrap();
    let create = requests.recv().unwrap();
    ui.reply(create.id, Err("cannot create".into()));
    ui.progress_issue();
    assert!(ui.issue_launch.is_none());
    assert!(requests.try_recv().is_err());
}

#[test]
fn context_change_cancels_issue_before_draining_old_session_response() {
    let (mut ui, requests, _) = connected();
    prepare(&mut ui);
    let selection = ui.assistant_capabilities().preferred();
    ui.start_issue(AssistantSession {
        selection,
        prompt: "Old workspace issue".into(),
    })
    .unwrap();
    let create = requests.recv().unwrap();
    ui.cancel_issue_start();
    ui.reply(create.id, Ok(json!({"id":"old-workspace-session"})));
    while let Ok(request) = requests.try_recv() {
        ui.reply(request.id, Ok(json!({})));
    }
    ui.progress_issue();
    assert!(requests.try_recv().is_err());
}

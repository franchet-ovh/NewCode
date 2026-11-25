use super::*;
use crate::api::Host;
use focus_addon_sdk::{GitOperation, Service, ServiceReply, View, Widget};
use std::collections::VecDeque;

#[derive(Default)]
struct Fake {
    replies: VecDeque<ServiceReply>,
    requests: Vec<Service>,
}
impl Fake {
    fn from(replies: Vec<ServiceReply>) -> Self {
        Self {
            replies: replies.into(),
            requests: vec![],
        }
    }
}
impl Host for Fake {
    fn service(&mut self, _: u64, service: Service) -> Result<ServiceReply, String> {
        self.requests.push(service);
        Ok(self.replies.pop_front().expect("Unexpected service call"))
    }
    fn publish(&mut self, _: &View) -> Result<(), String> {
        Ok(())
    }
}
fn ok(value: Value) -> ServiceReply {
    ServiceReply::success(value.to_string())
}
fn text(value: &str) -> ServiceReply {
    ServiceReply::success(value)
}
fn repo() -> Repo {
    Repo {
        remote: "origin".into(),
        owner: "owner".into(),
        name: "project".into(),
    }
}
fn app() -> App {
    let root = std::env::temp_dir().join(format!(
        "focus-github-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    App {
        repos: vec![repo()],
        metadata: json!({"default_branch":"main","_base":"main"}),
        context: Context {
            generation: 1,
            workspace: Some(root.clone()),
            storage: root,
            ..Default::default()
        },
        ..Default::default()
    }
}
fn action(id: &str, values: Value) -> Action {
    Action {
        generation: 1,
        id: id.into(),
        values: serde_json::from_value(values).unwrap(),
    }
}
fn pr(sha: &str, merged: bool) -> Value {
    json!({"number":42,"node_id":"pr-id","state":"open","head":{"sha":sha,"ref":"issue-42","repo":{"full_name":"owner/project"}},"base":{"ref":"main"},"merged_at":if merged {json!("today")} else {Value::Null}})
}
#[test]
fn recognizes_github_remotes_without_confusing_other_hosts() {
    for url in [
        "git@github.com:owner/project.git",
        "https://github.com/owner/project.git",
        "ssh://git@github.com/owner/project",
    ] {
        assert_eq!(parse_remote("origin", url), Some(repo()));
    }
    assert!(parse_remote("origin", "https://github.com.example/owner/project.git").is_none());
    assert!(parse_remote("origin", "https://github.com/owner/project/extra").is_none());
}
#[test]
fn falls_back_only_when_gh_is_missing() {
    let mut api = Api::default();
    let mut host = Fake::from(vec![
        ServiceReply::error("missing_tool", "missing"),
        ok(json!({"id":1})),
    ]);
    assert_eq!(
        api.request(&mut host, 1, "GET", "repos/owner/project", None)
            .unwrap()["id"],
        1
    );
    assert!(
        matches!(&host.requests[1], Service::Http { url, .. } if url == "https://api.github.com/repos/owner/project")
    );
    let mut api = Api::default();
    let mut host = Fake::from(vec![ServiceReply::error("command", "HTTP 403")]);
    assert!(
        api.request(&mut host, 1, "GET", "repos/owner/project", None)
            .is_err()
    );
    assert!(!api.no_gh);
    assert_eq!(host.requests.len(), 1);
}
#[test]
fn issue_list_excludes_pull_requests() {
    let mut app = app();
    let mut host = Fake::from(vec![
        ok(json!([{"number":1,"title":"Issue"},{"number":2,"pull_request":{}}])),
        text("main\n"),
    ]);
    app.refresh(&mut host, false).unwrap();
    assert_eq!(app.items.len(), 1);
    assert_eq!(app.items[0]["number"], 1);
}
#[test]
fn issue_branch_links_fetches_and_checks_out_without_creating_pr() {
    let mut app = app();
    app.detail = Detail::Item {
        value: json!({"number":42,"node_id":"issue-id"}),
        pull: false,
    };
    let mut host = Fake::from(vec![
        text(""),
        text(""),
        ServiceReply::error("command", "HTTP 404"),
        ok(json!({"commit":{"sha":"base-sha"}})),
        ok(json!({"data":{"createLinkedBranch":{}}})),
        text("fetched"),
        text("switched"),
    ]);
    app.issue_branch(
        &mut host,
        &action(
            "branch",
            json!({"branch":"issue-42","base":"main","reuse":false}),
        ),
    )
    .unwrap();
    assert!(host.requests.iter().any(
        |r| matches!(r, Service::Run{input:Some(body),..} if body.contains("createLinkedBranch"))
    ));
    assert!(
        matches!(host.requests.last().unwrap(), Service::Git {operation: GitOperation::Checkout {branch,..}} if branch == "issue-42")
    );
    assert_eq!(app.branch, "issue-42");
    let _ = fs::remove_dir_all(&app.context.storage);
}
#[test]
fn existing_issue_branch_requires_explicit_reuse() {
    let mut app = app();
    app.detail = Detail::Item {
        value: json!({"number":42,"node_id":"issue-id"}),
        pull: false,
    };
    let mut host = Fake::from(vec![text(""), text(""), ok(json!({"name":"issue-42"}))]);
    assert!(
        app.issue_branch(
            &mut host,
            &action("branch", json!({"branch":"issue-42","base":"main"}))
        )
        .unwrap_err()
        .contains("Reuse")
    );
    assert_eq!(host.requests.len(), 3);
}
#[test]
fn different_push_repository_is_rejected_before_push() {
    let mut app = app();
    app.detail = Detail::CreatePr {
        head: "issue-42".into(),
        title: "Fix".into(),
        body: String::new(),
    };
    let mut host = Fake::from(vec![text("git@github.com:other/project.git")]);
    let error = app
        .create_pr(
            &mut host,
            &action("create_pr", json!({"base":"main","title":"Fix","body":""})),
        )
        .unwrap_err();
    assert!(error.contains("different push repository"));
    assert_eq!(host.requests.len(), 1);
}
#[test]
fn pushes_before_creating_pr_and_links_associated_issue() {
    let mut app = app();
    app.detail = Detail::CreatePr {
        head: "issue-42".into(),
        title: "Fix".into(),
        body: String::new(),
    };
    app.stored
        .associations
        .insert(app.association(&repo(), "issue-42"), 42);
    let mut host = Fake::from(vec![
        text("git@github.com:owner/project.git\n"),
        text("issue-42\n"),
        ok(json!({})),
        text("pushed"),
        ok(json!([])),
        ok(pr("sha", false)),
    ]);
    app.create_pr(
        &mut host,
        &action(
            "create_pr",
            json!({"base":"main","title":"Fix issue","body":"Description"}),
        ),
    )
    .unwrap();
    assert!(matches!(
        &host.requests[3],
        Service::Git {
            operation: GitOperation::Push { .. }
        }
    ));
    assert!(
        matches!(host.requests.last().unwrap(), Service::Run { input:Some(body),..} if body.contains("Closes #42"))
    );
}
#[test]
fn changed_pr_head_blocks_merge_and_cleanup_is_opt_in() {
    let mut app = app();
    app.detail = Detail::Item {
        value: pr("old", false),
        pull: true,
    };
    assert!(
        all_widgets(&app.view(false).detail)
            .into_iter()
            .any(|w| matches!(w, Widget::Checkbox{id,checked:false,..} if id == "delete"))
    );
    let mut host = Fake::from(vec![ok(pr("new", false))]);
    assert!(
        app.merge(&mut host, &action("merge", json!({})))
            .unwrap_err()
            .contains("head changed")
    );
    assert_eq!(host.requests.len(), 1);
    app.detail = Detail::Item {
        value: pr("sha", false),
        pull: true,
    };
    let mut host = Fake::from(vec![
        ok(pr("sha", false)),
        text("scheduled"),
        ok(pr("sha", false)),
    ]);
    app.merge(&mut host, &action("merge", json!({"delete":false})))
        .unwrap();
    assert!(app.stored.cleanup.is_empty());
    assert!(
        matches!(&host.requests[1], Service::Run { args,..} if args.contains(&"--match-head-commit".into()) && !args.contains(&"--delete-branch".into()))
    );
}
#[test]
fn direct_api_respects_merge_queue_and_expected_head() {
    let mut app = app();
    app.api.no_gh = true;
    app.detail = Detail::Item {
        value: pr("sha", false),
        pull: true,
    };
    let mut host = Fake::from(vec![
        ok(pr("sha", false)),
        ok(
            json!({"data":{"node":{"isMergeQueueEnabled":true,"isInMergeQueue":false,"mergeStateStatus":"CLEAN"}}}),
        ),
        ok(json!({"data":{}})),
        ok(pr("sha", false)),
    ]);
    app.merge(&mut host, &action("merge", json!({}))).unwrap();
    assert!(
        matches!(&host.requests[2], Service::Http {body:Some(body),..} if body["query"].as_str().unwrap().contains("enqueuePullRequest") && body["variables"]["sha"] == "sha")
    );
}
#[test]
fn cleanup_waits_for_merge_and_preserves_remote_new_commits() {
    let mut app = app();
    app.stored.cleanup.push(Cleanup {
        repo: repo(),
        workspace: app.context.workspace.clone().unwrap(),
        number: 42,
        branch: "issue-42".into(),
        sha: "sha".into(),
        base: "main".into(),
        local_done: false,
        remote_done: false,
    });
    let mut host = Fake::from(vec![ok(pr("sha", false))]);
    app.check_cleanup(&mut host).unwrap();
    assert_eq!(host.requests.len(), 1);
    assert_eq!(app.stored.cleanup.len(), 1);
    let mut host = Fake::from(vec![
        ok(pr("sha", true)),
        ok(json!({"default_branch":"main"})),
        ok(json!({"commit":{"sha":"new-work"}})),
    ]);
    app.check_cleanup(&mut host).unwrap();
    assert_eq!(app.stored.cleanup.len(), 1);
    assert!(
        !host
            .requests
            .iter()
            .any(|r| matches!(r, Service::Git { .. }))
    );
    let _ = fs::remove_dir_all(&app.context.storage);
}

fn assistant_ready(app: &mut App, id: &str) {
    use focus_addon_sdk::{
        AssistantCapabilities, AssistantChoice, AssistantModel, InstalledAssistant,
    };
    app.update_assistants(vec![InstalledAssistant {
        id: id.into(),
        name: id.into(),
        enabled: true,
        error: None,
        capabilities: Some(AssistantCapabilities {
            ready: true,
            models: vec![AssistantModel {
                id: "model".into(),
                label: "Model".into(),
                reasoning: vec!["low".into(), "high".into()],
                default_reasoning: Some("low".into()),
            }],
            modes: vec![
                AssistantChoice {
                    id: "default".into(),
                    label: "Default".into(),
                },
                AssistantChoice {
                    id: "plan".into(),
                    label: "Plan".into(),
                },
            ],
            default_model: "model".into(),
            ..Default::default()
        }),
    }]);
    app.detail = Detail::Item {
        pull: false,
        value: json!({"number":42,"node_id":"issue-id",
        "title":"Fix startup", "body":"The container fails", "html_url":"https://github.com/owner/project/issues/42"}),
    };
}
fn assistant_action() -> Action {
    action(
        "issue_assistant",
        json!({"branch":"issue-42","base":"main","reuse":true,
        "assistant":"codex","assistant_model":"model","assistant_mode":"plan","assistant_reasoning":"high"}),
    )
}
#[test]
fn issue_session_uses_live_plan_high_choices_after_successful_checkout() {
    let mut app = app();
    assistant_ready(&mut app, "codex");
    assert_eq!(app.assistant_selection.mode, "plan");
    assert_eq!(app.assistant_selection.reasoning.as_deref(), Some("high"));
    let mut host = Fake::from(vec![
        text(""),
        text(""),
        ok(json!({"name":"issue-42"})),
        text("fetched"),
        text("switched"),
        text("starting"),
    ]);
    app.issue_assistant(&mut host, &assistant_action()).unwrap();
    assert!(matches!(
        &host.requests[4],
        Service::Git {
            operation: GitOperation::Checkout { .. }
        }
    ));
    let Service::StartAssistant { addon, session } = &host.requests[5] else {
        panic!("No AI handoff")
    };
    assert_eq!(addon, "codex");
    assert_eq!(session.selection.mode, "plan");
    assert_eq!(session.selection.reasoning.as_deref(), Some("high"));
    assert!(session.prompt.contains("owner/project"));
    assert!(session.prompt.contains("Issue #42: Fix startup"));
    assert!(session.prompt.contains("The container fails"));
    assert!(
        session
            .prompt
            .contains("https://github.com/owner/project/issues/42")
    );
    let _ = fs::remove_dir_all(&app.context.storage);
}
#[test]
fn missing_disabled_or_stale_assistant_options_do_not_change_branches() {
    let mut app = app();
    let mut host = Fake::default();
    assert!(app.issue_assistant(&mut host, &assistant_action()).is_err());
    assistant_ready(&mut app, "codex");
    app.assistants[0].enabled = false;
    assert!(
        app.issue_assistant(&mut host, &assistant_action())
            .unwrap_err()
            .contains("Enable")
    );
    app.assistants[0].enabled = true;
    app.assistants[0].capabilities.as_mut().unwrap().models[0].reasoning = vec!["low".into()];
    assert!(
        app.issue_assistant(&mut host, &assistant_action())
            .unwrap_err()
            .contains("reasoning")
    );
    assert!(host.requests.is_empty());
}
#[test]
fn cancelled_checkout_never_starts_an_assistant_session() {
    let mut app = app();
    assistant_ready(&mut app, "codex");
    let mut host = Fake::from(vec![
        text(""),
        text(""),
        ok(json!({"name":"issue-42"})),
        text("fetched"),
        ServiceReply::error("cancelled", "Checkout cancelled"),
    ]);
    assert!(
        app.issue_assistant(&mut host, &assistant_action())
            .unwrap_err()
            .contains("cancelled")
    );
    assert!(
        !host
            .requests
            .iter()
            .any(|s| matches!(s, Service::StartAssistant { .. }))
    );
    let _ = fs::remove_dir_all(&app.context.storage);
}
#[test]
fn switching_assistants_exposes_only_their_supported_options() {
    let mut app = app();
    assistant_ready(&mut app, "codex");
    let mut other = app.assistants[0].clone();
    other.id = "opencode".into();
    other.name = "OpenCode".into();
    let caps = other.capabilities.as_mut().unwrap();
    caps.models[0].id = "other".into();
    caps.models[0].reasoning.clear();
    caps.models[0].default_reasoning = None;
    caps.modes.retain(|m| m.id == "default");
    app.assistants.push(other);
    app.assistant_options(&action(
        "assistant_options",
        json!({"assistant":"opencode"}),
    ));
    assert_eq!(app.assistant_selection.model, "other");
    assert_eq!(app.assistant_selection.mode, "default");
    assert!(app.assistant_selection.reasoning.is_none());
    assert!(
        app.issue_assistant_widgets()
            .iter()
            .any(|w| matches!(w, Widget::Select{id,value,options,..}
                if id == "assistant_reasoning" && value.is_empty() && options.len() == 1))
    );
}

fn all_widgets(widgets: &[Widget]) -> Vec<&Widget> {
    let mut result = Vec::new();
    for widget in widgets {
        result.push(widget);
        if let Widget::Row { children, .. } = widget {
            result.extend(all_widgets(children));
        }
    }
    result
}

#[test]
fn pr_drafts_survive_navigation_and_do_not_cross_repositories() {
    let mut app = app();
    app.detail = Detail::CreatePr {
        head: "feature".into(),
        title: "Generated".into(),
        body: "Generated body".into(),
    };
    let mut host = Fake::default();
    app.perform(
        &mut host,
        action(
            "filter",
            json!({"title":"My draft", "body":"Unfinished description", "query":""}),
        ),
    )
    .unwrap();
    app.detail = Detail::None;
    app.detail = Detail::CreatePr {
        head: "feature".into(),
        title: "Generated".into(),
        body: "Generated body".into(),
    };
    let view = app.view(false);
    assert!(all_widgets(&view.detail).iter().any(|w| matches!(w, Widget::Input { id, value, .. } if id == "body" && value == "Unfinished description")));
    app.repos[0].name = "other".into();
    let view = app.view(false);
    assert!(all_widgets(&view.detail).iter().any(
        |w| matches!(w, Widget::Input { id, value, .. } if id == "title" && value == "Generated")
    ));
    assert!(
        host.requests.is_empty(),
        "Local filtering must not use GitHub API calls"
    );
}

#[test]
fn github_navigation_and_primary_actions_have_semantic_presentation() {
    let mut app = app();
    app.detail = Detail::Item {
        value: json!({"number":75,"title":"UI upgrade","body":"Details"}),
        pull: false,
    };
    app.items = vec![json!({"number":75,"title":"UI upgrade"})];
    let view = app.view(false);
    assert!(all_widgets(&view.sidebar).iter().any(
        |w| matches!(w, Widget::Button { id, style, .. } if id == "item:75" && style.selected)
    ));
    assert!(all_widgets(&view.detail).iter().any(|w| matches!(w, Widget::Button { id, style, .. } if id == "branch" && style.role == focus_addon_sdk::ButtonRole::Primary)));
}

use crate::{App, Detail, api::string};
use focus_addon_sdk::{ButtonRole as B, RowLayout as L, TextRole, View, Widget as W};

impl App {
    pub(crate) fn form_key(&self) -> String {
        let repo = self.repo().map(|r| r.slug()).unwrap_or_default();
        match &self.detail {
            Detail::None => format!("{repo}:{}:list", self.pulls),
            Detail::Item { value, pull } => format!("{repo}:{pull}:{}", value["number"]),
            Detail::CreatePr { head, .. } => format!("{repo}:create:{head}"),
        }
    }

    pub(crate) fn view(&self, busy: bool) -> View {
        let mut sidebar = Vec::new();
        if self.context.workspace.is_none() {
            sidebar.push(W::muted("Open a workspace to see GitHub issues."));
        } else if self.repos.is_empty() {
            sidebar.push(W::muted("No GitHub.com remote found in this workspace."));
        }
        if let Ok(repo) = self.repo() {
            sidebar.extend([
                W::Select {
                    on_change: Some("repository".into()),
                    id: "remote".into(),
                    label: "Repository".into(),
                    value: repo.remote.clone(),
                    options: self
                        .repos
                        .iter()
                        .map(|r| (r.remote.clone(), format!("{} ({})", r.slug(), r.remote)))
                        .collect(),
                },
                W::muted(format!("Branch: {}", self.branch)),
                W::row(
                    L::Wrap,
                    vec![
                        W::button("issues", "Issues").selected(!self.pulls),
                        W::button("pulls", "Pull requests").selected(self.pulls),
                    ],
                ),
                W::input("query", "Filter loaded items", &self.query).input_options(
                    "Search by title or number…",
                    "Filters the loaded results only.",
                    Some("filter"),
                ),
            ]);
            let needle = self.query.to_lowercase();
            let items: Vec<_> = self
                .items
                .iter()
                .filter(|item| {
                    format!("{} {}", item["number"], string(item, "title"))
                        .to_lowercase()
                        .contains(&needle)
                })
                .collect();
            if items.is_empty() && !busy {
                sidebar.push(W::muted(if self.query.is_empty() {
                    "No open items in the loaded results."
                } else {
                    "No matching items. Try another title or number."
                }));
            }
            for item in items {
                let number = item["number"].as_u64().unwrap_or_default();
                let selected = matches!(&self.detail, Detail::Item { value, pull } if *pull == self.pulls && value["number"].as_u64() == Some(number));
                let current = self.pulls && item["head"]["ref"].as_str() == Some(&self.branch);
                sidebar.push(
                    W::button(format!("item:{number}"), string(item, "title"))
                        .selected(selected)
                        .description(format!(
                            "#{number}{}",
                            if current { " · current branch" } else { "" }
                        )),
                );
            }
            if self.more {
                sidebar.push(W::button("more", "Load more"));
            }
            sidebar.extend([
                W::Separator,
                W::row(
                    L::Wrap,
                    vec![
                        W::button("refresh", "Refresh"),
                        W::button("new_pr", "Create pull request…").styled(B::Primary),
                    ],
                ),
            ]);
            let pending = self
                .stored
                .cleanup
                .iter()
                .filter(|i| Some(&i.workspace) == self.context.workspace.as_ref())
                .count();
            if pending > 0 {
                sidebar.push(W::status(
                    format!("{pending} pending branch cleanup operation(s)"),
                    TextRole::Warning,
                ));
                sidebar.push(W::button("cleanup", "Check merge and cleanup"));
            }
        }
        sidebar.push(W::Separator);
        sidebar.push(W::muted(if self.api.no_gh {
            "GitHub HTTPS API · OS keyring"
        } else {
            "gh login · OS keyring"
        }));
        sidebar.push(W::button("host.credentials", "Manage connection…").styled(B::Quiet));
        if !self.notice.is_empty() {
            sidebar.push(W::status(
                &self.notice,
                if self.notice_error {
                    TextRole::Error
                } else {
                    TextRole::Success
                },
            ));
        }
        let base = string(&self.metadata, "_base");
        let mut view = View {
            generation: self.context.generation,
            title: "GitHub".into(),
            form: self.form_key(),
            sidebar,
            detail: Vec::new(),
            busy,
            reveal_detail: false,
        };
        match &self.detail {
            Detail::None => (),
            Detail::Item { value, pull } => {
                let number = value["number"].as_u64().unwrap_or_default();
                view.title = format!("#{number} {}", string(value, "title"));
                view.detail.extend([
                    W::muted(self.repo().map(|r| r.slug()).unwrap_or_default()),
                    W::row(
                        L::Wrap,
                        vec![
                            W::status(string(value, "state"), TextRole::Success),
                            W::muted(format!("Opened by {}", string(&value["user"], "login"))),
                            W::Link {
                                label: "Open on GitHub".into(),
                                url: string(value, "html_url"),
                            },
                        ],
                    ),
                    W::row(
                        L::Card,
                        vec![
                            W::heading("About this item"),
                            W::Markdown {
                                text: string(value, "body"),
                            },
                        ],
                    ),
                ]);
                if *pull {
                    let mut review = vec![
                        W::heading("Review & merge"),
                        W::muted(format!(
                            "{} → {} · head {}",
                            string(&value["head"], "ref"),
                            string(&value["base"], "ref"),
                            string(&value["head"], "sha")
                        )),
                        W::Checkbox {
                            id: "delete".into(),
                            label: "Delete branches after merge".into(),
                            checked: false,
                        },
                    ];
                    if self.metadata["delete_branch_on_merge"].as_bool() == Some(true) {
                        review.push(W::muted("GitHub also has automatic branch deletion enabled for this repository."));
                    }
                    review.push(W::Button {
                        id: "merge".into(),
                        label: "Auto-merge (squash)".into(),
                        enabled: value["state"].as_str() == Some("open")
                            && value["draft"].as_bool() != Some(true),
                        style: focus_addon_sdk::ButtonStyle {
                            role: B::Primary,
                            ..Default::default()
                        },
                    });
                    view.detail.push(W::row(L::Card, review));
                } else {
                    view.detail.push(W::row(
                        L::Card,
                        vec![
                            W::heading("Start working on this issue"),
                            W::row(
                                L::Columns,
                                vec![
                                    W::input("branch", "Branch name", format!("issue-{number}")),
                                    W::input("base", "Base branch", &base),
                                ],
                            ),
                            W::Checkbox {
                                id: "reuse".into(),
                                label: "Reuse existing branch".into(),
                                checked: false,
                            },
                            W::button("branch", "Create issue branch & checkout")
                                .styled(B::Primary),
                        ],
                    ));
                    view.detail
                        .push(W::row(L::Card, self.issue_assistant_widgets()));
                }
            }
            Detail::CreatePr { head, title, body } => {
                view.title = "Create pull request".into();
                let remote = self
                    .repo()
                    .map(|r| format!("{} ({})", r.remote, r.slug()))
                    .unwrap_or_default();
                view.detail.push(W::row(
                    L::Card,
                    vec![
                        W::heading("Share your changes"),
                        W::muted(format!("Push {head} to {remote}")),
                        W::input("base", "Base branch", base),
                        W::input("title", "Title", title).input_options(
                            "A short summary of your change",
                            "",
                            None,
                        ),
                        W::Input {
                            id: "body".into(),
                            label: "Description".into(),
                            value: body.clone(),
                            multiline: true,
                            options: focus_addon_sdk::InputOptions {
                                placeholder: "What changed, and how did you check it?".into(),
                                help: "Markdown is supported. Your draft is kept while navigating."
                                    .into(),
                                ..Default::default()
                            },
                        },
                        W::button("create_pr", "Push & create pull request").styled(B::Primary),
                    ],
                ));
            }
        }
        if !view.detail.is_empty() {
            if busy {
                view.detail.insert(0, W::muted("Working…"));
            }
            if !self.notice.is_empty() {
                view.detail.push(W::status(
                    &self.notice,
                    if self.notice_error {
                        TextRole::Error
                    } else {
                        TextRole::Success
                    },
                ));
            }
        }
        if let Some(draft) = self.drafts.get(&view.form) {
            restore_draft(&mut view.detail, draft);
        }
        view
    }
}

fn restore_draft(
    widgets: &mut [W],
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) {
    for widget in widgets {
        match widget {
            W::Input { id, value, .. } => {
                if let Some(text) = values.get(id).and_then(|v| v.as_str()) {
                    *value = text.into();
                }
            }
            W::Checkbox { id, checked, .. } => {
                if let Some(value) = values.get(id).and_then(|v| v.as_bool()) {
                    *checked = value;
                }
            }
            W::Row { children, .. } => restore_draft(children, values),
            _ => (),
        }
    }
}

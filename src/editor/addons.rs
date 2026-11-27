use super::EditorApp;
use eframe::egui;

impl EditorApp {
    pub(super) fn addon_manager_central(&mut self, ui: &mut egui::Ui) -> bool {
        let visible = self
            .addons
            .manager_central(ui, &mut self.settings.addon_enabled);
        if let Some(update) = self.addons.take_settings_update() {
            let result = self.save_addon_settings(&update.addon, update.values);
            self.addons.settings_saved(result);
            ui.ctx().request_repaint();
        }
        visible
    }

    fn save_addon_settings(
        &mut self,
        id: &str,
        values: std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        if self.settings.workspace.is_none() {
            return Err("Open a workspace to configure this addon".into());
        }
        let previous = self.settings.addon_state.get(id).cloned();
        let mut state = previous
            .as_ref()
            .and_then(|s| s.as_object())
            .cloned()
            .unwrap_or_default();
        for (key, value) in values {
            let value = value.trim();
            if value.is_empty() {
                state.remove(&key);
            } else {
                state.insert(key, value.into());
            }
        }
        self.settings.addon_state.insert(id.into(), state.into());
        if let Err(error) = self.save_settings_result() {
            match previous {
                Some(state) => {
                    self.settings.addon_state.insert(id.into(), state);
                }
                None => {
                    self.settings.addon_state.remove(id);
                }
            }
            return Err(format!("Cannot save addon settings: {error}"));
        }
        self.addons
            .apply_settings(&self.settings.addon_enabled, &self.settings.addon_state);
        Ok(())
    }

    pub(super) fn update_addons(&mut self, ctx: &egui::Context) {
        self.addons
            .apply_settings(&self.settings.addon_enabled, &self.settings.addon_state);
        self.addons.manager_tick(ctx, &self.settings.addon_enabled);
        self.addons.sync(
            self.settings.workspace.as_deref(),
            self.dev_container.runtime(),
        );
        if self.addons.tick(
            ctx,
            self.dirty || !self.unsaved_files.is_empty(),
            self.sidebar_mode == super::SidebarMode::Addon
                && (!super::mobile::is_compact(ctx.viewport_rect().width())
                    || (!self.mobile.drawer_open
                        && matches!(
                            self.mobile.panel,
                            Some(super::mobile::MobilePanel::LeftAddon(_))
                        ))),
        ) {
            self.git_ui.external_change();
            self.explorer_cache.clear();
            // Checkout changes disk contents. Retain every unsaved editor buffer.
            let removed: Vec<_> = self
                .open_files
                .iter()
                .filter(|p| !p.is_file() && !self.file_is_dirty(p))
                .cloned()
                .collect();
            for path in removed {
                self.finish_close_file(path);
            }
            if !self.dirty {
                if let Some(path) = &self.active_file {
                    match std::fs::read_to_string(path) {
                        Ok(text) => {
                            self.disk_changes.remember(
                                path,
                                text.clone(),
                                crate::filesystem::DiskVersion::read(path),
                            );
                            self.buffer = text;
                        }
                        Err(error) => {
                            self.status =
                                format!("Could not reload file after Git operation: {error}")
                        }
                    }
                }
            }
        }
    }
    pub(super) fn addon_dialogs(&mut self, ctx: &egui::Context) {
        self.addons.credential_dialog(ctx);
        self.addons.manager_dialog(ctx);
        if !self.addons.checkout_waiting() {
            return;
        }
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Save files before changing branches")
            .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("A Git addon operation needs to change the worktree. Save unsaved files before continuing.");
                ui.horizontal(|ui| { save = ui.add(focus_addon_support::ui_theme::control_button("Save all and continue")).clicked(); cancel = ui.add(focus_addon_support::ui_theme::control_button("Cancel")).clicked(); });
            });
        if cancel {
            self.addons.confirm_checkout(false, ctx);
        }
        if save {
            let paths: Vec<_> = self
                .open_files
                .iter()
                .filter(|p| self.file_is_dirty(p))
                .cloned()
                .collect();
            if paths.iter().all(|path| self.save_document(path)) {
                self.addons.confirm_checkout(true, ctx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_app;
    use serde_json::json;

    #[test]
    fn addon_settings_preserve_state_and_are_saved_per_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.settings.workspace = Some(root.path().join("workspace-a"));
        app.settings_path = root.path().join("a/settings.toml");
        app.settings.addon_state.insert(
            "opencode".into(),
            json!({"autostart":false,"sessions":{"a":"session-a"}}),
        );
        app.settings
            .addon_state
            .insert("other".into(), json!({"keep":true}));
        app.save_addon_settings(
            "opencode",
            [("binary_path".into(), " /opt/custom tools/opencode ".into())].into(),
        )
        .unwrap();
        let read = || -> crate::settings::Settings {
            toml::from_str(&std::fs::read_to_string(&app.settings_path).unwrap()).unwrap()
        };
        let loaded = read();
        assert_eq!(
            loaded.addon_state["opencode"]["binary_path"],
            "/opt/custom tools/opencode"
        );
        assert_eq!(loaded.addon_state["opencode"]["autostart"], false);
        assert_eq!(loaded.addon_state["opencode"]["sessions"]["a"], "session-a");
        assert_eq!(loaded.addon_state["other"]["keep"], true);
        // Saving another workspace leaves the first workspace's path intact.
        app.settings.workspace = Some(root.path().join("workspace-b"));
        app.settings_path = root.path().join("b/settings.toml");
        app.settings.addon_state.clear();
        app.save_addon_settings(
            "opencode",
            [("binary_path".into(), "/container/bin/opencode".into())].into(),
        )
        .unwrap();
        let a: crate::settings::Settings =
            toml::from_str(&std::fs::read_to_string(root.path().join("a/settings.toml")).unwrap())
                .unwrap();
        assert_eq!(
            a.addon_state["opencode"]["binary_path"],
            "/opt/custom tools/opencode"
        );
        app.save_addon_settings("opencode", [("binary_path".into(), " ".into())].into())
            .unwrap();
        assert!(
            app.settings.addon_state["opencode"]
                .get("binary_path")
                .is_none()
        );
    }

    #[test]
    fn failed_addon_settings_save_rolls_back_state_and_requires_a_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut app = test_app();
        let values = || [("binary_path".into(), "/new/opencode".into())].into();
        assert!(
            app.save_addon_settings("opencode", values())
                .unwrap_err()
                .contains("workspace")
        );
        app.settings.workspace = Some(root.path().into());
        let blocker = root.path().join("file");
        std::fs::write(&blocker, "not a directory").unwrap();
        app.settings_path = blocker.join("settings.toml");
        assert!(app.save_addon_settings("opencode", values()).is_err());
        assert!(!app.settings.addon_state.contains_key("opencode"));
        app.settings.addon_state.insert(
            "opencode".into(),
            json!({"binary_path":"/old/opencode","autostart":false}),
        );
        let previous = app.settings.addon_state.clone();
        assert!(
            app.save_addon_settings("opencode", values())
                .unwrap_err()
                .contains("Cannot save addon settings")
        );
        assert_eq!(app.settings.addon_state, previous);
    }
}

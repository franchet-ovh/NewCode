//! Native viewer of the same authoritative session used by browser viewers.
use super::{
    native_worker::{CloseAction, CloseResult, Command, Event, Worker},
    protocol::{Control, Frame, Input},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use eframe::egui;
use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc,
    time::Duration,
};

struct Painter {
    textures: HashMap<String, egui::TextureHandle>,
    meshes: Vec<(egui::Rect, String, egui::Mesh)>,
    size: egui::Vec2,
    free: Vec<String>,
}
impl Default for Painter {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            meshes: vec![],
            free: vec![],
            size: egui::vec2(1440.0, 900.0),
        }
    }
}
impl Painter {
    fn update(&mut self, ctx: &egui::Context, frame: &Frame) -> Result<(), String> {
        for id in self.free.drain(..) {
            self.textures.remove(&id);
        }
        if frame.reset {
            self.textures.clear();
        }
        for texture in &frame.textures {
            let bytes = STANDARD
                .decode(&texture.pixels)
                .map_err(|_| "Invalid texture")?;
            let image = egui::ColorImage::new(
                texture.size,
                bytes
                    .chunks_exact(4)
                    .map(|pixel| {
                        egui::Color32::from_rgba_premultiplied(
                            pixel[0], pixel[1], pixel[2], pixel[3],
                        )
                    })
                    .collect(),
            );
            if let Some(pos) = texture.pos {
                if let Some(handle) = self.textures.get_mut(&texture.id) {
                    handle.set_partial(pos, image, texture.options);
                }
            } else if let Some(handle) = self.textures.get_mut(&texture.id) {
                handle.set(image, texture.options);
            } else {
                self.textures.insert(
                    texture.id.clone(),
                    ctx.load_texture("editor-session", image, texture.options),
                );
            }
        }
        self.meshes.clear();
        for mesh in &frame.meshes {
            let vertices = STANDARD
                .decode(&mesh.vertices)
                .map_err(|_| "Invalid vertices")?;
            let indices = STANDARD
                .decode(&mesh.indices)
                .map_err(|_| "Invalid indices")?;
            let vertices = vertices
                .chunks_exact(20)
                .map(|bytes| {
                    let float = |at| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                    egui::epaint::Vertex {
                        pos: egui::pos2(float(0), float(4)),
                        uv: egui::pos2(float(8), float(12)),
                        color: egui::Color32::from_rgba_premultiplied(
                            bytes[16], bytes[17], bytes[18], bytes[19],
                        ),
                    }
                })
                .collect();
            let indices = indices
                .chunks_exact(4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
                .collect();
            self.meshes.push((
                egui::Rect::from_min_max(
                    egui::pos2(mesh.clip[0], mesh.clip[1]),
                    egui::pos2(mesh.clip[2], mesh.clip[3]),
                ),
                mesh.texture.clone(),
                egui::Mesh {
                    vertices,
                    indices,
                    ..Default::default()
                },
            ));
        }
        self.free = frame.free.clone();
        self.size = egui::vec2(frame.size[0], frame.size[1]);
        Ok(())
    }
    fn draw(&self, ui: &mut egui::Ui) -> egui::emath::TSTransform {
        let rect = ui.available_rect_before_wrap();
        let scale = (rect.width() / self.size.x).min(rect.height() / self.size.y);
        let offset = rect.center() - self.size * scale * 0.5;
        let transform = egui::emath::TSTransform::new(offset.to_vec2(), scale);
        for (clip, texture, mesh) in &self.meshes {
            if let Some(texture) = self.textures.get(texture) {
                let mut mesh = mesh.clone();
                mesh.texture_id = texture.id();
                mesh.transform(transform);
                ui.painter()
                    .with_clip_rect((transform * *clip).intersect(rect))
                    .add(egui::Shape::mesh(mesh));
            }
        }
        transform
    }
}

/// Do not feed presentation-only repaints back into the authoritative editor.
#[derive(Default)]
struct InputTracker(Option<Input>);
impl InputTracker {
    fn changed(&mut self, input: &Input) -> bool {
        let mut state = input.clone();
        state.events.clear();
        let changed = !input.events.is_empty() || self.0.as_ref() != Some(&state);
        self.0 = Some(state);
        changed
    }
}

struct NativeApp {
    worker: Worker,
    pending: VecDeque<Command>,
    painter: Painter,
    frame: Option<Frame>,
    input: InputTracker,
    close_confirmation: bool,
    close_pending: bool,
    close_error: Option<String>,
    failure: Option<String>,
    confirmed: bool,
}
impl NativeApp {
    fn new(ctx: &egui::Context, args: &crate::logging::Arguments) -> Result<Self, String> {
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        let wake = ctx.clone();
        let worker = Worker::start(args, move || wake.request_repaint())?;
        Ok(Self {
            worker,
            pending: VecDeque::new(),
            painter: Painter::default(),
            frame: None,
            input: InputTracker::default(),
            close_confirmation: false,
            close_pending: false,
            close_error: None,
            failure: None,
            confirmed: false,
        })
    }
    fn control(&self) -> Control {
        self.frame
            .as_ref()
            .map_or(self.worker.control.clone(), |frame| frame.control.clone())
    }
    fn flush(&mut self, ctx: &egui::Context) {
        while let Some(command) = self.pending.pop_front() {
            match self.worker.sender.try_send(command) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(command)) => {
                    // Preserve key/button boundaries under backpressure without blocking
                    // the GUI or dropping input. Only a full queue requires a timed retry.
                    self.pending.push_front(command);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
    }
    fn close(&mut self, action: CloseAction) {
        self.close_pending = true;
        self.close_error = None;
        self.pending.push_back(Command::Close(action));
    }
    fn accept(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Frame(frame) => {
                let control_changed = self.control() != frame.control;
                if frame.title
                    != self
                        .frame
                        .as_ref()
                        .and_then(|previous| previous.title.clone())
                {
                    if let Some(title) = &frame.title {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                    }
                }
                let reset = if let Err(error) = self.painter.update(ctx, &frame) {
                    log::warn!("Native viewer: {error}");
                    true
                } else {
                    false
                };
                if frame.control.owned {
                    for command in &frame.output.commands {
                        match command {
                            egui::OutputCommand::CopyText(text) => ctx.copy_text(text.clone()),
                            egui::OutputCommand::OpenUrl(url) => ctx.open_url(url.clone()),
                            egui::OutputCommand::CopyImage(image) => ctx.copy_image(image.clone()),
                        }
                    }
                    if frame.paste {
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    }
                }
                self.frame = Some(*frame);
                self.pending.push_back(Command::Applied { reset });
                if control_changed {
                    // Synchronize this window's dimensions/focus after ownership changes.
                    ctx.request_repaint();
                }
            }
            Event::Close(result) => {
                self.close_pending = false;
                match result {
                    CloseResult::Allowed => {
                        self.confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    CloseResult::Dirty => self.close_confirmation = true,
                    CloseResult::SaveFailed => {
                        self.close_confirmation = true;
                        self.close_error =
                            Some("Could not save all documents. The editor remains open.".into());
                    }
                }
            }
            Event::Dialog(purpose, directory) => {
                let path = crate::file_picker::pick_native(&purpose, &directory);
                self.pending.push_back(Command::DialogResult(purpose, path));
            }
            Event::Failed(error) => {
                log::error!("Native editor session failed: {error}");
                self.failure = Some(error);
            }
            Event::Stopped => {
                self.confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}
impl eframe::App for NativeApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let control = self.control();
        let size = raw
            .screen_rect
            .unwrap_or_else(|| ctx.viewport_rect())
            .size();
        let accepts_input = control.owned
            && !self.close_confirmation
            && !self.close_pending
            && self.failure.is_none();
        let input = Input {
            size: [size.x.max(1.0), size.y.max(1.0)],
            pixels_per_point: raw
                .viewport()
                .native_pixels_per_point
                .unwrap_or(1.0)
                .clamp(0.5, 4.0),
            focused: raw.focused,
            events: if accepts_input {
                raw.events.clone()
            } else {
                vec![]
            },
            epoch: if accepts_input {
                Some(control.epoch)
            } else {
                None
            },
        };
        if self.input.changed(&input) {
            self.pending.push_back(Command::Input(input));
        }
        if accepts_input {
            raw.events.clear();
        }
        if raw.viewport().close_requested() && self.failure.is_some() {
            self.confirmed = true;
        }
        if raw.viewport().close_requested() && !self.confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.close_pending && !self.close_confirmation {
                // The worker checks dirty state after processing all preceding input,
                // even when its newest frame has not reached this window yet.
                self.close(CloseAction::Check);
            }
        }
    }
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let events: Vec<_> = self.worker.incoming.try_iter().take(64).collect();
        for event in events {
            self.accept(ctx, event);
        }
        self.flush(ctx);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let owned = self.control().owned;
        if !owned {
            egui::Panel::top("viewer_control").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("This editor is controlled remotely");
                    if ui.button("Take control").clicked() {
                        self.pending.push_back(Command::Claim);
                    }
                });
            });
        }
        let transform = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.painter.draw(ui))
            .inner;
        if owned {
            if let Some(frame) = &self.frame {
                ctx.set_cursor_icon(frame.output.cursor_icon);
                ctx.output_mut(|output| {
                    output.ime = frame.output.ime.map(|mut ime| {
                        ime.rect = transform * ime.rect;
                        ime.cursor_rect = transform * ime.cursor_rect;
                        ime
                    })
                });
            }
        }
        if self.close_confirmation {
            egui::Modal::new(egui::Id::new("close_native_editor")).show(&ctx, |ui| {
                ui.heading("Unsaved changes");
                ui.label("Save changes before closing this editor?");
                if let Some(error) = &self.close_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.add_enabled_ui(!self.close_pending, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Save all").clicked() {
                            self.close(CloseAction::Save);
                        }
                        if ui.button("Discard").clicked() {
                            self.close(CloseAction::Discard);
                        }
                        if ui.button("Cancel").clicked() {
                            self.close_confirmation = false;
                            self.close_error = None;
                        }
                    });
                });
            });
        }
        if let Some(error) = self.failure.clone() {
            egui::Modal::new(egui::Id::new("native_session_failed")).show(&ctx, |ui| {
                ui.heading("Editor session stopped");
                ui.label(error);
                if ui.button("Close").clicked() {
                    self.confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
        // UI actions are submitted in this pass rather than waiting for another repaint.
        self.flush(&ctx);
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // The worker also saves periodically and on drop if its queue is busy.
        let _ = self.worker.sender.try_send(Command::SaveSettings);
    }
}

pub(super) fn run(arguments: &crate::logging::Arguments) -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        persist_window: false,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Focus Editor",
        options,
        Box::new(|cc| {
            Ok(Box::new(NativeApp::new(&cc.egui_ctx, arguments).map_err(
                |error| std::io::Error::other(error.to_string()),
            )?))
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_repaints_do_not_resubmit_input_but_events_and_view_changes_do() {
        let mut tracker = InputTracker::default();
        let mut input = Input {
            size: [1200.0, 800.0],
            pixels_per_point: 1.0,
            focused: true,
            events: vec![],
            epoch: Some(2),
        };
        assert!(tracker.changed(&input));
        assert!(!tracker.changed(&input));
        input.events.push(egui::Event::Text("a".into()));
        assert!(tracker.changed(&input));
        assert!(tracker.changed(&input)); // Repeated text must not be deduplicated.
        input.events.clear();
        assert!(!tracker.changed(&input));
        input.size[0] = 1000.0;
        assert!(tracker.changed(&input));
        input.pixels_per_point = 2.0;
        assert!(tracker.changed(&input));
        input.focused = false;
        assert!(tracker.changed(&input));
        input.epoch = Some(3);
        assert!(tracker.changed(&input));
        input.epoch = None;
        assert!(tracker.changed(&input));
        assert!(!tracker.changed(&input));
    }
}

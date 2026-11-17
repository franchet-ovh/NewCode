//! Reusable event loop for independently compiled addons contributing a UI surface.
use crate::{surface, ui_theme};
use egui;
use focus_addon_sdk::{
    AssistantCapabilities, AssistantSession, Context, Message, PROTOCOL_VERSION, ServiceReply,
    read_message, write_message,
};
use std::{io::BufReader, sync::mpsc, time::Duration};

pub trait Panel {
    fn context(&mut self, context: Context, ctx: &egui::Context);
    fn tick(&mut self, ctx: &egui::Context);
    fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context);
    fn state(&self) -> String;
    fn assistant_capabilities(&self) -> Option<AssistantCapabilities> {
        None
    }
    fn start_assistant(
        &mut self,
        _session: AssistantSession,
        _ctx: &egui::Context,
    ) -> Result<(), String> {
        Err("This addon does not support starting assistant sessions".into())
    }
}
pub fn run(mut panel: impl Panel) -> Result<(), String> {
    let level = std::env::var("FOCUS_ADDON_LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let _ = env_logger::Builder::new()
        .parse_filters(&format!("warn,focus_addon={level}"))
        .format(|buf, record| {
            use std::io::Write;
            writeln!(buf, "{} {}", record.level(), record.args())
        })
        .try_init();
    let ctx = egui::Context::default();
    ui_theme::apply(&ctx);
    let (tx, rx) = mpsc::sync_channel(128);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(std::io::stdin());
        while let Ok(Some(message)) = read_message(&mut reader) {
            if tx.send(message).is_err() {
                return;
            }
        }
    });
    let mut output = std::io::stdout();
    let mut generation = 0;
    let mut last_state = String::new();
    let mut last_capabilities = None;
    loop {
        let message = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panel.tick(&ctx);
                report(
                    &panel,
                    &mut last_state,
                    &mut last_capabilities,
                    generation,
                    &mut output,
                )?;
                continue;
            }
            Err(_) => break,
        };
        match message.method.as_deref() {
            Some("initialize") => {
                let params = message.params.unwrap_or_default();
                if params["protocol"].as_u64() != Some(PROTOCOL_VERSION as u64) {
                    return Err("Unsupported addon protocol".into());
                }
                write_message(&mut output,&Message::response(message.id.unwrap_or(0),serde_json::json!({"protocol":PROTOCOL_VERSION,"surface":surface::SURFACE_VERSION}))).map_err(|e|e.to_string())?;
                let context: Context =
                    serde_json::from_value(params["context"].clone()).map_err(|e| e.to_string())?;
                generation = context.generation;
                panel.context(context, &ctx);
            }
            Some("context.changed") => {
                last_state.clear();
                last_capabilities = None;
                let context: Context = serde_json::from_value(message.params.unwrap_or_default())
                    .map_err(|e| e.to_string())?;
                generation = context.generation;
                ctx.memory_mut(|m| *m = Default::default());
                panel.context(context, &ctx);
            }
            Some("assistant.start") => {
                if let Some(id) = message.id {
                    let params = message.params.unwrap_or_default();
                    let result = if params["generation"].as_u64() != Some(generation) {
                        Err("Workspace changed".into())
                    } else {
                        panel.tick(&ctx);
                        serde_json::from_value::<AssistantSession>(params["session"].clone())
                            .map_err(|e| e.to_string())
                            .and_then(|session| panel.start_assistant(session, &ctx))
                    };
                    let reply = match result {
                        Ok(()) => ServiceReply::success("New assistant session is starting"),
                        Err(error) => ServiceReply::error("assistant", error),
                    };
                    write_message(&mut output, &Message::response(id, reply))
                        .map_err(|e| e.to_string())?;
                }
            }
            Some("surface.input") => {
                let input: surface::Input =
                    serde_json::from_value(message.params.unwrap_or_default())
                        .map_err(|e| e.to_string())?;
                if input.generation != generation {
                    continue;
                }
                panel.tick(&ctx);
                let frame = surface::render(&ctx, input, panel.state(), |ui| panel.show(ui, &ctx));
                write_message(&mut output, &Message::notification("surface.frame", frame))
                    .map_err(|e| e.to_string())?;
            }
            Some("shutdown") => break,
            _ => panel.tick(&ctx),
        }
        report(
            &panel,
            &mut last_state,
            &mut last_capabilities,
            generation,
            &mut output,
        )?;
    }
    Ok(())
}

fn report(
    panel: &impl Panel,
    last: &mut String,
    last_capabilities: &mut Option<AssistantCapabilities>,
    generation: u64,
    output: &mut impl std::io::Write,
) -> Result<(), String> {
    let capabilities = panel.assistant_capabilities();
    if &capabilities != last_capabilities {
        write_message(
            output,
            &Message::notification(
                "assistant.capabilities",
                serde_json::json!({"generation":generation,"capabilities":capabilities}),
            ),
        )
        .map_err(|e| e.to_string())?;
        *last_capabilities = capabilities;
    }
    let state = panel.state();
    if &state != last {
        write_message(
            output,
            &Message::notification(
                "panel.status",
                serde_json::json!({"generation":generation,"state":state}),
            ),
        )
        .map_err(|e| e.to_string())?;
        *last = state;
    }
    Ok(())
}

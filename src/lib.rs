mod filesystem;
use focus_addon_support::assistant_ui;
mod addons;
mod app_paths;
mod browser;
mod dev_container;
mod editor;
mod file_picker;
mod git_ui;
mod indentation;
mod launch;
mod logging;
mod lsp;
mod platform;
mod repository_jobs;
mod server;
mod settings;
mod syntax;
mod terminal;
mod terminal_fonts;
mod ui_theme;
mod workspaces;

#[cfg(test)]
use eframe::egui;

pub fn editor_main() -> Result<(), Box<dyn std::error::Error>> {
    platform::prepare_environment();
    let result = run();
    if result.is_err() {
        log::error!("Application terminated with an error");
    }
    result
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = std::env::args_os().skip(1);
    if raw.next().as_deref() == Some(std::ffi::OsStr::new("--check-addon-compatibility")) {
        let path = raw.next().ok_or("Expected an addon.toml path")?;
        if raw.next().is_some() {
            return Err("Expected one addon.toml path".into());
        }
        let manifest: focus_addon_sdk::Manifest = toml::from_str(&std::fs::read_to_string(path)?)?;
        focus_addon_sdk::check_compatibility(
            manifest.editor_version_req.as_deref(),
            manifest.protocol,
            env!("CARGO_PKG_VERSION"),
        )?;
        return Ok(());
    }
    let arguments = logging::Arguments::parse(std::env::args_os().skip(1))?;
    if arguments.version {
        println!("focus-editor {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Err("Focus Editor supports Linux and macOS".into());
    }
    if arguments.headless
        && !arguments
            .workspace
            .as_deref()
            .is_some_and(|path| path.is_dir())
    {
        return Err("Headless workspace must be an existing directory".into());
    }
    if !arguments.ui && !arguments.headless {
        browser::validate_runtime()?;
    }
    if launch::detach()? {
        return Ok(());
    }
    logging::init(&arguments)?;
    log::debug!("Launch arguments validated");
    log::info!("Application starting version={}", env!("CARGO_PKG_VERSION"));
    if arguments.headless {
        browser::run_headless(&arguments)?;
    } else if !arguments.ui {
        browser::run(&arguments)?;
    } else {
        browser::run_native(&arguments)?;
    }
    log::info!("Application stopped");
    Ok(())
}

// Headless frames have no renderer to apply texture uploads to.
#[cfg(test)]
fn test_frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    render: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, render);
    output.textures_delta.clear();
    output
}

/// Start the manually managed browser gateway.
pub fn server_main() -> Result<(), Box<dyn std::error::Error>> {
    platform::prepare_environment();
    server::run()
}

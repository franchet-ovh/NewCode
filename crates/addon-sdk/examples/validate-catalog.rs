//! Lightweight publisher validation without building the editor.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let catalog: focus_addon_sdk::Catalog = serde_json::from_reader(std::io::stdin())?;
    catalog.validate()?;
    Ok(())
}

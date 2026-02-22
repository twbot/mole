use anyhow::{Context, Result};
use dialoguer::FuzzySelect;
use dialoguer::theme::ColorfulTheme;

/// Show a fuzzy picker and return the selected item's index.
pub fn pick(prompt: &str, items: &[String]) -> Result<usize> {
    if items.is_empty() {
        anyhow::bail!("no tunnels available");
    }

    FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(items)
        .interact()
        .context("selection cancelled")
}

use rgsp_ui::ui::Ui;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // `Ui::new()`/`Drop` bracket the display, the same way Task 3's
    // `InitSettings`/`show_display`/`QuitSettings` did — except now the
    // release is structural: Drop runs even if a panic unwinds through the
    // body below.
    let mut ui = Ui::new()?;
    let (w, h) = ui.size();
    tracing::info!(w, h, "display up");

    tracing::info!("waiting for B");
    loop {
        ui.begin();
        ui.header("rgsp-ui");
        ui.row("First option", None, 0, false);
        ui.row("Second option", Some("on"), 1, true);
        ui.row("Third option", Some("42"), 2, false);
        ui.hints(&[("A", "SELECT"), ("B", "BACK")]);
        ui.end();

        let buttons = ui.poll();
        if buttons.b {
            break;
        }
    }

    Ok(())
}

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = AppWindow::new()?;
    let weak = app.as_weak();
    let handle = runtime.handle().clone();

    app.on_probe(move || {
        let weak = weak.clone();
        handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let _ = weak.upgrade_in_event_loop(|app| {
                app.set_probe_status("后台任务已返回；尚未连接 Codex 或上游".into());
            });
        });
    });

    app.run()?;
    Ok(())
}

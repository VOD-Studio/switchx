//! Operation results shown in the window's shared feedback area.

use switchx::ui::AppWindow;

pub(super) fn show_action(app: &AppWindow, result: Result<String, String>) {
    app.set_busy(false);
    show_action_feedback(app, result);
}

pub(super) fn show_action_feedback(app: &AppWindow, result: Result<String, String>) {
    match result {
        Ok(message) => {
            app.set_action_message(message.into());
            app.set_error_code("".into());
            app.set_error_message("".into());
            app.set_error_action("".into());
        }
        Err(message) => {
            app.set_error_code("operation_failed".into());
            app.set_error_message(message.into());
            app.set_error_action("检查输入或刷新后重试；发生配置冲突时先检查目标文件。".into());
        }
    }
}

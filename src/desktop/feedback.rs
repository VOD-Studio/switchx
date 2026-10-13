//! Operation results shown in the window's shared feedback area.

use switchx::{ui::AppWindow, upstream::TRANSIENT_HELPER_MESSAGE};

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
            // Retrying cannot help until SwitchX is relaunched from Applications.
            let (code, action) = if message == TRANSIENT_HELPER_MESSAGE {
                ("transient_app_location", "未修改 Codex 配置。")
            } else {
                (
                    "operation_failed",
                    "检查输入或刷新后重试；发生配置冲突时先检查目标文件。",
                )
            };
            app.set_error_code(code.into());
            app.set_error_message(message.into());
            app.set_error_action(action.into());
        }
    }
}

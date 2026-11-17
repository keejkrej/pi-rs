//! Port of packages/coding-agent/examples/extensions/notify.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    AgentSettledEvent, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
};

fn windows_toast_script(title: &str, body: &str) -> String {
    let win_type = "Windows.UI.Notifications";
    let mgr = format!("[{win_type}.ToastNotificationManager, {win_type}, ContentType = WindowsRuntime]");
    let template = format!("[{win_type}.ToastTemplateType]::ToastText01");
    let toast = format!("[{win_type}.ToastNotification]::new($xml)");
    [
        format!("{mgr} > $null"),
        format!("$xml = [{win_type}.ToastNotificationManager]::GetTemplateContent({template})"),
        format!("$xml.GetElementsByTagName('text')[0].AppendChild($xml.CreateTextNode('{body}')) > $null"),
        format!("[{win_type}.ToastNotificationManager]::CreateToastNotifier('{title}').Show({toast})"),
    ]
    .join("; ")
}

fn notify_osc777(title: &str, body: &str) {
    pi_js::console::stdout_write(&format!("\x1b]777;notify;{title};{body}\x07"));
}

fn notify_osc99(title: &str, body: &str) {
    pi_js::console::stdout_write(&format!("\x1b]99;i=1:d=0;{title}\x1b\\"));
    pi_js::console::stdout_write(&format!("\x1b]99;i=1:p=body;{body}\x1b\\"));
}

fn notify_windows(title: &str, body: &str) {
    let script = windows_toast_script(title, body);
    let _ = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .spawn();
}

fn notify(title: &str, body: &str) {
    if pi_js::env::var("WT_SESSION").is_some_and(|v| !v.is_empty()) {
        notify_windows(title, body);
    } else if pi_js::env::var("KITTY_WINDOW_ID").is_some_and(|v| !v.is_empty()) {
        notify_osc99(title, body);
    } else {
        notify_osc777(title, body);
    }
}

pub struct NotifyExtension;

#[async_trait]
impl Extension for NotifyExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<AgentSettledEvent>(Arc::new(|_event, _ctx| {
            Box::pin(async move {
                notify("Pi", "Ready for input");
                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(NotifyExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}

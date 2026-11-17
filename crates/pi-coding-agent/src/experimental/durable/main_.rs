//! Port of packages/coding-agent/src/experimental/durable/main.ts

use crate::experimental::durable::runtime::{OpenDurableOptions, open_durable};
use crate::experimental::durable::tui::run_durable_tui;

pub fn parse_args(argv: &[String]) -> pi_js::Result<OpenDurableOptions> {
    let mut continue_session = false;
    for arg in argv {
        if arg == "--continue" || arg == "-c" {
            continue_session = true;
        } else {
            return Err(pi_js::Error::msg(format!("Unknown argument: {arg}")));
        }
    }
    Ok(OpenDurableOptions {
        cwd: None,
        continue_session,
    })
}

pub async fn run_main(argv: &[String]) -> pi_js::Result<()> {
    let opts = parse_args(argv)?;
    let durable = open_durable(opts).await?;
    let tui_res = run_durable_tui(&durable.view, &durable.controller, &durable.settings).await;
    let close_res = durable.close().await;
    tui_res?;
    close_res
}

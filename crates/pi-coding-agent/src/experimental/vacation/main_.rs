//! Port of packages/coding-agent/src/experimental/vacation/main.ts

pub use crate::experimental::durable::main_::parse_args;
use crate::experimental::vacation::runtime::open_durable;
use crate::experimental::vacation::tui::run_durable_tui;

pub async fn run_main(argv: &[String]) -> pi_js::Result<()> {
    let opts = parse_args(argv)?;
    let durable = open_durable(opts).await?;
    let tui_res = run_durable_tui(&durable.view, &durable.controller, &durable.settings).await;
    let close_res = durable.close().await;
    tui_res?;
    close_res
}

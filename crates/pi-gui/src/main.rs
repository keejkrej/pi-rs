//! Binary entry point for `pi-gui`.

use std::process::ExitCode;

fn print_help() {
    pi_js::console::log(
        "\
pi-gui - Native desktop GUI for pi-rs

USAGE:
    pi-gui [OPTIONS]

OPTIONS:
    --cwd <dir>               Set working directory
    --session <id|path>       Resume session by ID or JSONL file path
    --session-dir <dir>       Custom session storage directory
    --provider <id>           Initial provider ID
    --model <id>              Initial model ID
    --thinking <level>        Thinking level (off|minimal|low|medium|high|xhigh)
    --theme <mode>            Theme override (dark|light|system)
    --demo                    Launch with offline deterministic showcase data
    --scene <id>              Render a deterministic scene (dark-session, light-session, diff-inspector, command-palette, settings-plugins)
    --snap <path>             Render headless frame to PNG via Rune and exit
    --reduced-motion          Disable spring motion and transitions
    -V, --version             Print version information
    -h, --help                Print this help message",
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-V" || a == "--version") {
        pi_js::console::log(&format!("pi-gui {}", env!("CARGO_PKG_VERSION")));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }
    rune::set_app_id("dev.pi.gui");
    let mut spec = pi_gui::app_spec(pi_gui::LaunchConfig::default());
    spec.scenes = pi_gui::SCENE_ENTRIES;
    rune::run(spec);
    ExitCode::SUCCESS
}

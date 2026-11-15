//! Port of packages/tui/test/tui-cell-size-input.test.ts

mod support;

use std::sync::{Arc, Mutex};

use pi_tui::terminal_image::{
    CellDimensions, ImageProtocol, TerminalCapabilities, get_cell_dimensions, reset_capabilities_cache,
    set_capabilities, set_cell_dimensions,
};
use pi_tui::tui::Component;
use pi_tui::tui_main_screen::TuiMainScreen;
use serial_test::serial;

use support::virtual_terminal::VirtualTerminal;

struct InputRecorder {
    inputs: Mutex<Vec<String>>,
}

impl InputRecorder {
    fn new() -> Self {
        Self {
            inputs: Mutex::new(Vec::new()),
        }
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }
}

impl Component for InputRecorder {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![String::new()]
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        self.inputs.lock().unwrap().push(data.to_string());
    }

    fn invalidate(&self) {}
}

struct ImageCapabilityGuard {
    previous_cell_dims: CellDimensions,
}

impl ImageCapabilityGuard {
    fn enable_kitty_images() -> Self {
        let previous_cell_dims = get_cell_dimensions();
        reset_capabilities_cache();
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: false,
        });
        Self { previous_cell_dims }
    }
}

impl Drop for ImageCapabilityGuard {
    fn drop(&mut self) {
        set_cell_dimensions(self.previous_cell_dims);
        reset_capabilities_cache();
    }
}

#[tokio::test]
#[serial]
async fn passes_lone_escape_through_without_delay_when_cell_size_query_is_pending() {
    let _guard = ImageCapabilityGuard::enable_kitty_images();
    let terminal = Arc::new(VirtualTerminal::new(80, 24));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);
    let recorder = Arc::new(InputRecorder::new());
    tui.add_child(recorder.clone());
    tui.set_focus(Some(recorder.clone()));

    tui.start();
    terminal.send_input("\x1b");

    assert_eq!(recorder.inputs(), vec!["\x1b".to_string()]);
    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn consumes_valid_cell_size_response_and_still_handles_subsequent_keys() {
    let _guard = ImageCapabilityGuard::enable_kitty_images();
    set_cell_dimensions(CellDimensions {
        width_px: 9,
        height_px: 18,
    });
    let terminal = Arc::new(VirtualTerminal::new(80, 24));
    let tui = TuiMainScreen::new(terminal.clone(), None, None);
    let recorder = Arc::new(InputRecorder::new());
    tui.add_child(recorder.clone());
    tui.set_focus(Some(recorder.clone()));

    tui.start();
    terminal.send_input("\x1b[6;20;10t");

    assert!(recorder.inputs().is_empty());
    assert_eq!(
        get_cell_dimensions(),
        CellDimensions {
            width_px: 10,
            height_px: 20,
        }
    );

    terminal.send_input("q");
    assert_eq!(recorder.inputs(), vec!["q".to_string()]);
    tui.stop(None);
}

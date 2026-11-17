//! Port of packages/coding-agent/src/modes/interactive/components/earendil-announcement.ts

use std::sync::{Arc, OnceLock};

use pi_tui::{Component, Container, Image, ImageOptions, ImageTheme, Spacer, Text, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;

const BLOG_URL: &str = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";
const IMAGE_FILENAME: &str = "clankolas.png";

static CACHED_IMAGE_BASE64: OnceLock<Option<String>> = OnceLock::new();

fn load_image_base64() -> Option<&'static str> {
    CACHED_IMAGE_BASE64
        .get_or_init(|| {
            let bytes =
                pi_js::fs::read_file(&crate::config::get_bundled_interactive_asset_path(IMAGE_FILENAME)).ok()?;
            Some(pi_js::b64::encode(&bytes))
        })
        .as_deref()
}

/// Announcement component shown when `/earendil` or the Earendil announcement banner is displayed.
#[derive(Clone)]
pub struct EarendilAnnouncementComponent {
    container: Container,
}

impl Default for EarendilAnnouncementComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl EarendilAnnouncementComponent {
    pub fn new() -> Self {
        let t = theme();
        let container = Container::new();

        container.add_child(Arc::new(DynamicBorder::new(Some(Arc::new(|text| {
            theme().fg(ThemeColor::Accent, text)
        })))));
        container.add_child(Arc::new(Text::new(
            Some(&t.bold(&t.fg(ThemeColor::Accent, "pi has joined Earendil"))),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Muted, "Read the blog post:")),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::MdLink, BLOG_URL)),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        if let Some(image_base64) = load_image_base64() {
            container.add_child(Arc::new(Image::new(
                image_base64,
                "image/png",
                ImageTheme {
                    fallback_color: Arc::new(|text| theme().fg(ThemeColor::Muted, text)),
                },
                Some(ImageOptions {
                    max_width_cells: Some(56),
                    max_height_cells: None,
                    filename: Some(IMAGE_FILENAME.to_string()),
                    image_id: None,
                }),
                None,
            )));
            container.add_child(Arc::new(Spacer::new(Some(1))));
        }

        container.add_child(Arc::new(DynamicBorder::new(Some(Arc::new(|text| {
            theme().fg(ThemeColor::Accent, text)
        })))));

        Self { container }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    pub fn invalidate(&self) {
        self.container.invalidate();
    }
}

impl Component for EarendilAnnouncementComponent {
    fn render(&self, width: usize) -> Vec<String> {
        EarendilAnnouncementComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        EarendilAnnouncementComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.container.children())
    }
}

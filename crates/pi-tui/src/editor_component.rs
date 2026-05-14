use crate::tui::Component;

pub trait EditorComponent: Component {
    fn get_text(&self) -> &str;
    fn set_text(&mut self, text: String);
    fn cursor(&self) -> usize;
}

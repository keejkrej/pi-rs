//! Port of packages/tui/src/layout-node.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::tui::Component;

/// PORT: JS `Symbol.for("@earendil-works/pi-tui/layout-node")`. Capability is [`LayoutComponent`];
/// the string is the symbol key.
pub const LAYOUT_NODE: &str = "@earendil-works/pi-tui/layout-node";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutViewport {
    pub width: i64,
    pub height: i64,
}

/// `"vstack" | "hstack"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackLayoutType {
    #[serde(rename = "vstack")]
    VStack,
    #[serde(rename = "hstack")]
    HStack,
}

/// `"stretch" | "start" | "center" | "end"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackAlign {
    #[serde(rename = "stretch")]
    Stretch,
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "center")]
    Center,
    #[serde(rename = "end")]
    End,
}

/// `number | "auto"`.
#[derive(Clone, Debug, PartialEq)]
pub enum StackBasis {
    Fixed(f64),
    Auto,
}

/// `"chain" | "contain"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollOverscroll {
    #[serde(rename = "chain")]
    Chain,
    #[serde(rename = "contain")]
    Contain,
}

#[derive(Clone)]
pub struct StackLayoutEntry {
    pub component: Arc<dyn Component>,
    pub basis: Option<StackBasis>,
    pub grow: Option<i64>,
    pub shrink: Option<i64>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub visible: Option<Arc<dyn Fn(&LayoutViewport) -> bool + Send + Sync>>,
}

/// PORT: `LayoutNode::Stack` is the `type: "scroll"` complement. `r#type` is `"vstack" | "hstack"`.
/// `entries` is the snapshot returned by one `[LAYOUT_NODE]()` call (TS returns the live array).
#[derive(Clone)]
pub struct StackLayoutNode {
    pub r#type: StackLayoutType,
    pub entries: Vec<StackLayoutEntry>,
    pub gap: i64,
    pub align: StackAlign,
}

/// Live scroll-view state returned on a scroll [`LayoutNode`].
///
/// PORT: TS passes the `ScrollView` itself (`state: this`).
pub trait ScrollLayoutState: Send + Sync {
    fn scroll_top(&self) -> i64;
    fn primary(&self) -> bool;
    fn overscroll(&self) -> ScrollOverscroll;
    fn viewport_height(&self) -> i64;
    fn get_content_width(&self, width: i64) -> i64;
    fn update_layout(&self, content_height: i64, viewport_height: i64, request_render: Arc<dyn Fn() + Send + Sync>);
}

#[derive(Clone)]
pub struct ScrollLayoutNode {
    pub component: Arc<dyn Component>,
    pub state: Arc<dyn ScrollLayoutState>,
}

#[derive(Clone)]
pub enum LayoutNode {
    Stack(StackLayoutNode),
    Scroll(ScrollLayoutNode),
}

pub trait LayoutComponent: Component {
    fn layout_node(&self) -> LayoutNode;
}

pub fn get_layout_node(component: &dyn Component) -> Option<LayoutNode> {
    component.as_layout_component().map(|layout| layout.layout_node())
}

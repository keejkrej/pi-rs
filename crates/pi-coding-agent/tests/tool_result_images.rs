//! Port of packages/coding-agent/test/tool-result-images.test.ts
//!
//! WIRE(@mcp-w3): Blocked on `utils::tool_result_images::normalize_tool_result_images` landing.

#[test]
#[ignore = "WIRE(@mcp-w3): waiting on utils::tool_result_images::normalize_tool_result_images"]
fn normalize_tool_result_images_pending_mcp_w3() {}

#[cfg(any())]
mod pending_mcp_w3 {
    // Full test specs to wire upon @mcp-w3 merge:
    // - returns original array when there are no image blocks
    // - returns original array when images are already within limits
    // - resizes oversized images and reports original dimensions
    // - leaves oversized images alone when autoResizeImages is disabled
    // - converts BMP attachments to PNG and emits conversion notice
}

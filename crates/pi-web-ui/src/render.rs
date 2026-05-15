use crate::format::format_usage;
use pi_agent::messages::{
    AgentMessage, AssistantContentBlock, TextContent, UserContentBlock, assistant_content_to_text,
    user_content_to_text,
};

#[derive(Debug, Clone)]
pub struct ChatPanelOptions {
    pub title: String,
    pub include_css: bool,
}

impl Default for ChatPanelOptions {
    fn default() -> Self {
        Self {
            title: "pi".to_string(),
            include_css: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatPanel {
    messages: Vec<AgentMessage>,
    options: ChatPanelOptions,
}

impl ChatPanel {
    pub fn new(options: ChatPanelOptions) -> Self {
        Self {
            messages: Vec::new(),
            options,
        }
    }

    pub fn push(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }

    pub fn render(&self) -> String {
        render_chat_panel(&self.messages, &self.options)
    }
}

pub fn render_chat_panel(messages: &[AgentMessage], options: &ChatPanelOptions) -> String {
    let mut html = String::new();
    html.push_str("<section class=\"pi-chat-panel\">\n");
    html.push_str(&format!(
        "<header class=\"pi-chat-header\">{}</header>\n",
        escape_html(&options.title)
    ));
    html.push_str("<main class=\"pi-message-list\">\n");
    for message in messages {
        html.push_str(&render_message(message));
        html.push('\n');
    }
    html.push_str("</main>\n</section>");

    if options.include_css {
        format!("<style>{}</style>\n{}", default_css(), html)
    } else {
        html
    }
}

pub fn render_message(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User { content, .. } => {
            render_block("user", "User", &render_user_content(content))
        }
        AgentMessage::Assistant {
            content,
            usage,
            model,
            ..
        } => {
            let body = render_assistant_content(content);
            let meta = format!(
                "<footer>{} · {}</footer>",
                escape_html(model),
                format_usage(usage)
            );
            render_block("assistant", "Assistant", &(body + &meta))
        }
        AgentMessage::ToolResult {
            tool_name,
            content,
            is_error,
            ..
        } => {
            let class = if *is_error { "tool error" } else { "tool" };
            render_block(
                class,
                &format!("Tool: {tool_name}"),
                &render_user_content(content),
            )
        }
        AgentMessage::CompactionSummary { summary, .. } => {
            render_block("summary", "Compaction", &escape_html(summary))
        }
        AgentMessage::BranchSummary { summary, .. } => {
            render_block("summary", "Branch summary", &escape_html(summary))
        }
    }
}

fn render_block(class: &str, label: &str, body: &str) -> String {
    format!(
        "<article class=\"pi-message {}\"><h3>{}</h3><div class=\"pi-message-body\">{}</div></article>",
        escape_html(class),
        escape_html(label),
        body,
    )
}

fn render_user_content(content: &[UserContentBlock]) -> String {
    escape_html(&user_content_to_text(content)).replace('\n', "<br>")
}

fn render_assistant_content(content: &[AssistantContentBlock]) -> String {
    let mut html = String::new();
    for block in content {
        match block {
            AssistantContentBlock::Text(TextContent { text, .. }) => {
                html.push_str("<p>");
                html.push_str(&escape_html(text).replace('\n', "<br>"));
                html.push_str("</p>");
            }
            AssistantContentBlock::Thinking(thinking) => {
                html.push_str("<details class=\"pi-thinking\"><summary>Thinking</summary><pre>");
                html.push_str(&escape_html(&thinking.thinking));
                html.push_str("</pre></details>");
            }
            AssistantContentBlock::ToolCall(call) => {
                html.push_str("<pre class=\"pi-tool-call\">");
                html.push_str(&escape_html(&format!("{} {}", call.name, call.arguments)));
                html.push_str("</pre>");
            }
        }
    }
    if html.is_empty() {
        escape_html(&assistant_content_to_text(content))
    } else {
        html
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn default_css() -> &'static str {
    r#"
.pi-chat-panel{height:100%;display:flex;flex-direction:column;background:#0b0f17;color:#e5e7eb;font:14px system-ui,sans-serif}
.pi-chat-header{padding:12px 16px;border-bottom:1px solid #1f2937;font-weight:700}
.pi-message-list{overflow:auto;padding:16px;display:flex;flex-direction:column;gap:12px}
.pi-message{border:1px solid #1f2937;border-radius:12px;padding:12px;background:#111827}
.pi-message.user{background:#172033}.pi-message.assistant{background:#10151f}.pi-message.tool{background:#0f1f1b}.pi-message.error{border-color:#7f1d1d}.pi-message.summary{opacity:.85}
.pi-message h3{margin:0 0 8px;font-size:12px;color:#93c5fd;text-transform:uppercase;letter-spacing:.08em}.pi-message p{margin:0 0 8px}.pi-message footer{margin-top:8px;color:#9ca3af;font-size:12px}.pi-message pre{white-space:pre-wrap;overflow:auto}
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_message_html() {
        let html = render_message(&AgentMessage::User {
            content: vec![UserContentBlock::Text(TextContent::new("<script>"))],
            timestamp: 1,
        });
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }
}

//! Port of packages/coding-agent/test/agent-session-queued-images.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_ai::types::{ImageContent, Message, TranscriptContext, UserContentBlock, UserMessageContent};
use pi_coding_agent::core::agent_session::{PromptOptions, QueueInputOptions, StreamingBehavior};
use pi_coding_agent::core::session_manager::SessionEntry;
use support::suite::harness::{HarnessOptions, create_harness};

fn create_gray_png_b64(width: u32, height: u32) -> String {
    let pixels = vec![0x80u8; (width as usize) * (height as usize) * 3];
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&pixels, width, height, ColorType::Rgb8.into())
        .expect("encode png");
    pi_js::b64::encode(&out)
}

fn persisted_user_content(entries: &[SessionEntry], text: &str) -> Vec<UserContentBlock> {
    for entry in entries {
        let SessionEntry::Message(msg_entry) = entry else {
            continue;
        };
        let AgentMessage::Message(Message::User(user)) = &msg_entry.message else {
            continue;
        };
        let UserMessageContent::Blocks(ref blocks) = user.content else {
            continue;
        };
        if let Some(UserContentBlock::Text(t)) = blocks.first()
            && t.text.lines().next() == Some(text)
        {
            return blocks.clone();
        }
    }
    panic!("No user message \"{text}\"");
}

async fn wait_for(mut cond: impl FnMut() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for condition");
}

// Regression test for #11738: queued messages skipped the image resize idle prompts get.
#[tokio::test]
async fn resizes_images_in_steering_follow_up_and_streaming_prompt_messages() {
    let harness = create_harness(HarnessOptions::default()).await;
    let recorded_contexts = Arc::new(Mutex::new(Vec::<TranscriptContext>::new()));

    let mk_step = |label: &'static str, delay_ms: u64, contexts: Arc<Mutex<Vec<TranscriptContext>>>| {
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _state, _model| {
            let contexts = Arc::clone(&contexts);
            Box::pin(async move {
                contexts.lock().unwrap().push(ctx);
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(label.to_string()),
                    None,
                ))
            })
        }))
    };

    harness.set_responses(vec![
        mk_step("first", 300, Arc::clone(&recorded_contexts)),
        mk_step("second", 0, Arc::clone(&recorded_contexts)),
        mk_step("third", 0, Arc::clone(&recorded_contexts)),
        mk_step("fourth", 0, Arc::clone(&recorded_contexts)),
    ]);

    let image = ImageContent::new(create_gray_png_b64(1200, 9000), "image/png");
    let session = harness.session.clone();
    let run_handle = tokio::spawn(async move { session.prompt("idle", None).await });

    wait_for(|| harness.session.is_streaming()).await;
    harness
        .session
        .steer("steer", Some(vec![image.clone()]), Some(QueueInputOptions::default()))
        .await
        .expect("steer");
    harness
        .session
        .follow_up(
            "follow up",
            Some(vec![image.clone()]),
            Some(QueueInputOptions::default()),
        )
        .await
        .expect("follow_up");
    harness
        .session
        .prompt(
            "streaming prompt",
            Some(PromptOptions {
                images: Some(vec![image.clone()]),
                streaming_behavior: Some(StreamingBehavior::FollowUp),
                ..Default::default()
            }),
        )
        .await
        .expect("streaming prompt");

    run_handle.await.expect("join prompt").expect("prompt ok");
    wait_for(|| !harness.session.is_streaming() && harness.faux.state().call_count() >= 4).await;

    let entries = harness.session_manager.get_entries();
    for text in ["steer", "follow up", "streaming prompt"] {
        let blocks = persisted_user_content(&entries, text);
        let images: Vec<&ImageContent> = blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Image(img) => Some(img),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 1, "for {text}");
        assert_ne!(images[0].data, image.data, "image should be resized for {text}");
        let UserContentBlock::Text(first_text) = &blocks[0] else {
            panic!("expected text block");
        };
        assert!(
            first_text.text.contains("original 1200x9000, displayed at 267x2000"),
            "text for {text}: {}",
            first_text.text
        );
    }

    let sent_contexts = recorded_contexts.lock().unwrap().clone();
    let mut sent_images = Vec::new();
    for ctx in &sent_contexts {
        for msg in &ctx.messages {
            match msg {
                Message::User(u) => {
                    if let UserMessageContent::Blocks(ref blocks) = u.content {
                        for b in blocks {
                            if let UserContentBlock::Image(img) = b {
                                sent_images.push(img.clone());
                            }
                        }
                    }
                }
                Message::ToolResult(tr) => {
                    for b in &tr.content {
                        if let UserContentBlock::Image(img) = b {
                            sent_images.push(img.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    assert!(!sent_images.is_empty());
    assert!(sent_images.iter().all(|img| img.data != image.data));
    assert_eq!(harness.session.pending_message_count(), 0);

    harness.cleanup();
}

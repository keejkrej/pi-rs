//! Port of packages/coding-agent/test/experimental-transcript-provider.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::ServiceObject;
use pi_chord::{Facet, FacetOptions, create_facet_host, define_facet};
use pi_coding_agent::experimental::services::transcript::{TRANSCRIPT_SERVICE, Transcript};
use pi_coding_agent::experimental::services::transcript_provider::create_transcript_service_facet;
use pi_durable::{ConversationView, SubmissionDraft, UserInput};

use support::experimental_durable_support::open_faux_conversation;

#[tokio::test]
async fn replicates_the_conversation_view_as_it_changes() {
    let msg = faux_assistant_message(FauxAssistantMessageContent::Text("answer".to_string()), None);
    let conv = open_faux_conversation(vec![FauxResponseStep::Message(msg)])
        .await
        .expect("open faux");
    let views: Arc<Mutex<Vec<ConversationView>>> = Arc::new(Mutex::new(Vec::new()));
    let views_sub = Arc::clone(&views);

    let consumer = define_facet(Facet::new(
        "test-transcript-consumer",
        Arc::new(move |env| {
            let transcript_obj = env.r#use::<ServiceObject>(&*TRANSCRIPT_SERVICE)?;
            let views_inner = Arc::clone(&views_sub);
            let env_activate = env.clone();
            env.on_activate(Arc::new(move || {
                let transcript_obj = transcript_obj.clone();
                let views_inner = Arc::clone(&views_inner);
                let env_activate = env_activate.clone();
                Box::pin(async move {
                    let transcript = Transcript::from_service(transcript_obj)?;
                    let unsub = transcript.state.subscribe(move |value, _ctx, _del| {
                        views_inner.lock().expect("lock").push(value);
                        Box::pin(futures::future::ready(Ok(())))
                    })?;
                    let unsub = std::sync::Mutex::new(Some(unsub));
                    env_activate.own(Arc::new(move || {
                        if let Some(u) = unsub.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            u.call();
                        }
                        Box::pin(futures::future::ready(Ok(())))
                    }))
                })
            }))
        }),
    ));

    let provider_facet = create_transcript_service_facet(conv.conversation.clone(), BACKGROUND_CONTEXT.clone())
        .await
        .expect("create transcript facet");

    let host = create_facet_host(FacetOptions {
        facets: vec![provider_facet, consumer],
        service_sources: None,
        on_error: None,
    })
    .await
    .expect("create host");

    {
        let initial = views.lock().expect("lock");
        assert!(!initial.is_empty());
        assert!(initial[0].entries.is_empty());
    }

    let submission = conv
        .conversation
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("question".to_string()),
                when_busy: None,
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("submit");
    let _ = submission.wait(BACKGROUND_CONTEXT.clone()).await.expect("wait");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let latest_kinds: Option<Vec<String>> = {
            let guard = views.lock().expect("lock");
            guard.last().map(|v| v.entries.iter().map(|e| e.kind.clone()).collect())
        };
        if latest_kinds.as_deref() == Some(&["pi.user".to_string(), "pi.assistant".to_string()]) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for replicated entries: {latest_kinds:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    host.dispose().await.expect("dispose host");
    conv.close().await.expect("close");
}

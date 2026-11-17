//! Port of packages/coding-agent/src/experimental/services/transcript-provider.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{Context, Facet, define_facet};
use pi_durable::Conversation;

use crate::experimental::services::connection::{TypedReplicatedState, json_value_from_serde};
use crate::experimental::services::transcript::{TRANSCRIPT_SERVICE, Transcript};

/// Serve the conversation's durable view state. The facet owns and disposes the attached state.
pub async fn create_transcript_service_facet(conversation: Conversation, context: Context) -> pi_js::Result<Facet> {
    let attached = conversation.view_state(context).await?;
    Ok(define_facet(Facet::new(
        "@pi/transcript",
        Arc::new(move |env| {
            let initial_json = json_value_from_serde(&attached.value())?;
            let raw_state = env.replicated_state(initial_json)?;
            let typed_state = TypedReplicatedState::from_local_raw(raw_state.clone());

            let state_sub = raw_state.clone();
            let unsub = attached.subscribe(Arc::new(move |next_view, ctx, _delivery| {
                let state = state_sub.clone();
                Box::pin(async move {
                    let next_json = json_value_from_serde(&next_view)?;
                    state.replace(ctx, next_json)
                })
            }));
            // Sync any updates published between view_state() resolution and subscription install.
            let latest_json = json_value_from_serde(&attached.value())?;
            let _ = raw_state.replace(BACKGROUND_CONTEXT.clone(), latest_json);

            let attached_dispose = attached.clone();
            let unsub_cell = Arc::new(Mutex::new(Some(unsub)));
            env.own(Arc::new(move || {
                let unsub = unsub_cell.lock().unwrap_or_else(PoisonError::into_inner).take();
                let attached_dispose = attached_dispose.clone();
                Box::pin(async move {
                    if let Some(u) = unsub {
                        u.call();
                    }
                    attached_dispose.dispose()
                })
            }))?;

            let service_obj = Transcript::from_state(typed_state).into_service_object();
            env.provide(&*TRANSCRIPT_SERVICE, service_obj)
        }),
    )))
}

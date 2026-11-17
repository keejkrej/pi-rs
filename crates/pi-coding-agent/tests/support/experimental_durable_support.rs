//! Port of packages/coding-agent/test/experimental-durable-support.ts

use std::sync::Arc;

use futures::FutureExt;
use futures::future::Shared;
use pi_ai::models::create_models;
use pi_ai::{FauxProviderHandle, FauxResponseStep, RegisterFauxProviderOptions, faux_provider};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::{
    AgentChange, Conversation, FieldChange, Harness, HarnessOptions, MemoryStorage, ModelRef, RootConversationOptions,
    create_registry,
};
use pi_js::BoxFuture;
use tokio::sync::oneshot;

pub struct FauxConversation {
    pub faux: FauxProviderHandle,
    pub harness: Harness,
    pub conversation: Conversation,
}

impl FauxConversation {
    pub async fn close(&self) -> pi_js::Result<()> {
        self.harness.close(BACKGROUND_CONTEXT.clone()).await
    }
}

/// An in-memory durable Harness whose root conversation answers with the faux provider.
pub async fn open_faux_conversation(responses: Vec<FauxResponseStep>) -> pi_js::Result<FauxConversation> {
    let faux = faux_provider(Some(RegisterFauxProviderOptions::default()));
    faux.set_responses(responses);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models,
            registry: create_registry(),
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        BACKGROUND_CONTEXT.clone(),
    )
    .await?;
    let model = faux.get_model();
    let conversation = harness
        .root(
            BACKGROUND_CONTEXT.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::set(ModelRef {
                        provider: model.provider,
                        model_id: model.id,
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;
    Ok(FauxConversation {
        faux,
        harness,
        conversation,
    })
}

pub struct PendingResponse {
    pub step: FauxResponseStep,
    pub reached: Shared<BoxFuture<()>>,
}

/// A faux response that never arrives; it ends only when its generation is aborted.
pub fn pending_response() -> PendingResponse {
    let (tx, rx) = oneshot::channel::<()>();
    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
    let reached: Shared<BoxFuture<()>> = async move {
        let _ = rx.await;
    }
    .boxed()
    .shared();

    let step = FauxResponseStep::Factory(Arc::new(move |_ctx, options, _state, _model| {
        let tx = Arc::clone(&tx);
        Box::pin(async move {
            if let Some(sender) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(());
            }
            let signal = options
                .and_then(|o| o.base.signal)
                .ok_or_else(|| pi_js::Error::msg("Pending response requires abort signal"))?;
            if let Err(err) = signal.throw_if_aborted() {
                return Err(err);
            }
            signal.cancelled().await;
            match signal.reason() {
                Some(reason) => Err(pi_js::Error::Abort(reason)),
                None => Err(pi_js::Error::msg("The operation was aborted")),
            }
        })
    }));

    PendingResponse { step, reached }
}

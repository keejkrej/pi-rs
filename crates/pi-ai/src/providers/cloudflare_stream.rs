//! Port of packages/ai/src/providers/cloudflare-stream.ts

use std::sync::Arc;

use crate::types::{
    ClassifierFunction, ClassifierModel, ImageModel, Model, ProviderClassifier, ProviderEnv, ProviderStreams,
    SimpleStreamFunction, StreamFunction,
};

const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

/// Model descriptor whose Cloudflare template endpoint can be resolved from `ProviderEnv`.
pub trait CloudflareEndpointModel: Clone {
    fn base_url(&self) -> &str;
    fn set_base_url(&mut self, base_url: String);
}

impl CloudflareEndpointModel for Model {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn set_base_url(&mut self, base_url: String) {
        self.base_url = base_url;
    }
}

impl CloudflareEndpointModel for ClassifierModel {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn set_base_url(&mut self, base_url: String) {
        self.base_url = base_url;
    }
}

impl CloudflareEndpointModel for ImageModel {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn set_base_url(&mut self, base_url: String) {
        self.base_url = base_url;
    }
}

pub fn resolve_cloudflare_model<TModel: CloudflareEndpointModel>(model: &TModel, env: Option<&ProviderEnv>) -> TModel {
    let Some(env) = env else {
        return model.clone();
    };
    let original = model.base_url();
    let account_token = format!("{{{CLOUDFLARE_ACCOUNT_ID}}}");
    let gateway_token = format!("{{{CLOUDFLARE_GATEWAY_ID}}}");

    let with_account = match env.get(CLOUDFLARE_ACCOUNT_ID) {
        Some(account_id) => original.replace(&account_token, account_id),
        None => original.to_owned(),
    };
    let base_url = match env.get(CLOUDFLARE_GATEWAY_ID) {
        Some(gateway_id) => with_account.replace(&gateway_token, gateway_id),
        None => with_account,
    };

    if base_url == original {
        model.clone()
    } else {
        let mut next = model.clone();
        next.set_base_url(base_url);
        next
    }
}

/// Wrap an API implementation so Cloudflare account/gateway endpoint placeholders materialize
/// from the resolved provider env before dispatch.
pub fn cloudflare_streams(streams: ProviderStreams) -> ProviderStreams {
    let stream_inner = Arc::clone(&streams.stream);
    let stream: StreamFunction = Arc::new(move |model, context, options| {
        let resolved = resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.env.as_ref()));
        (stream_inner)(&resolved, context, options)
    });

    let simple_inner = Arc::clone(&streams.stream_simple);
    let stream_simple: SimpleStreamFunction = Arc::new(move |model, context, options| {
        let resolved = resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.env.as_ref()));
        (simple_inner)(&resolved, context, options)
    });

    ProviderStreams {
        stream,
        stream_simple,
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

/// Classifier counterpart of [`cloudflare_streams`].
pub fn cloudflare_classifier(classifier: ProviderClassifier) -> ProviderClassifier {
    let classify_inner = Arc::clone(&classifier.classify);
    let classify: ClassifierFunction = Arc::new(move |model, context, options| {
        let resolved = resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.base.env.as_ref()));
        (classify_inner)(&resolved, context, options)
    });

    ProviderClassifier { classify }
}

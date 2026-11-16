//! Port of packages/ai/src/providers/opencode-headers.ts

use std::sync::Arc;

use crate::types::{
    ApiStreamOptions, ProviderHeaders, ProviderStreams, SimpleStreamFunction, SimpleStreamOptions, StreamFunction,
    StreamOptions,
};

const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    headers
        .map(|map| map.keys().any(|key| key.eq_ignore_ascii_case(name)))
        .unwrap_or(false)
}

fn inject_session_header(base: &mut StreamOptions) {
    let Some(session_id) = base.session_id.as_ref().filter(|s| !s.is_empty()).cloned() else {
        return;
    };
    if has_header(base.headers.as_ref(), OPENCODE_SESSION_HEADER) {
        return;
    }
    let headers = base.headers.get_or_insert_with(ProviderHeaders::new);
    headers.insert(OPENCODE_SESSION_HEADER.to_owned(), Some(session_id));
}

fn with_api_session_header(options: Option<ApiStreamOptions>) -> Option<ApiStreamOptions> {
    options.map(|mut opts| {
        inject_session_header(&mut opts.base);
        opts
    })
}

fn with_simple_session_header(options: Option<SimpleStreamOptions>) -> Option<SimpleStreamOptions> {
    options.map(|mut opts| {
        inject_session_header(&mut opts.base);
        opts
    })
}

/// Adds OpenCode's required per-conversation routing header before API dispatch.
pub fn with_open_code_session_header(streams: ProviderStreams) -> ProviderStreams {
    let stream_inner = Arc::clone(&streams.stream);
    let stream: StreamFunction =
        Arc::new(move |model, context, options| (stream_inner)(model, context, with_api_session_header(options)));

    let simple_inner = Arc::clone(&streams.stream_simple);
    let stream_simple: SimpleStreamFunction =
        Arc::new(move |model, context, options| (simple_inner)(model, context, with_simple_session_header(options)));

    ProviderStreams {
        stream,
        stream_simple,
        fetch_deferred: streams.fetch_deferred,
        cancel_deferred: streams.cancel_deferred,
    }
}

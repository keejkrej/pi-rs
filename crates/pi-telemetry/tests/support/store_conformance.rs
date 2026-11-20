//! Port of packages/telemetry/test/store-conformance.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js::BoxFuture;
use pi_telemetry::memory::testing::MockClock;
use pi_telemetry::{
    AttributeValue, MetricKind, MetricQuery, Result, SpanAttributes, SpanLink, SpanOptions, SpanStatus, SpanValue,
    TelemetryCarrier, TelemetryContext, TelemetryMetrics, TelemetryPropagation, TelemetryStore, TraceQuery,
};
use pretty_assertions::assert_eq;
use regex::Regex;

#[async_trait]
pub trait StoreFixture: Send + Sync {
    fn context(&self) -> Arc<dyn TelemetryContext + Send + Sync>;
    fn metrics(&self) -> Arc<dyn TelemetryMetrics + Send + Sync>;
    fn propagation(&self) -> Arc<dyn TelemetryPropagation + Send + Sync>;
    fn store(&self) -> Arc<dyn TelemetryStore + Send + Sync>;
    async fn flush(&self) -> Result<()>;
    async fn async_dispose(&self) -> Result<()>;
}

pub type StoreFixtureFactory = Arc<dyn Fn() -> BoxFuture<Result<Box<dyn StoreFixture>>> + Send + Sync>;

pub async fn returns_only_settled_traces_complete_span_snapshots_and_detached_results(factory: &StoreFixtureFactory) {
    let _clock = MockClock::new(Some(20_000.0), None);
    let f = factory().await.expect("create fixture");

    let gate = Arc::new(tokio::sync::Notify::new());
    let gate_cb = Arc::clone(&gate);
    let mut root_attrs = SpanAttributes::new();
    root_attrs.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["original".to_string()]),
    );

    let ctx = f.context();
    let active = tokio::spawn(async move {
        ctx.start_span(
            SpanOptions {
                name: "root".to_string(),
                attributes: Some(root_attrs),
                links: None,
            },
            Arc::new(move |root| {
                let gate_cb = Arc::clone(&gate_cb);
                Box::pin(async move {
                    let mut event_attrs = SpanAttributes::new();
                    event_attrs.insert(
                        "tags".to_string(),
                        AttributeValue::StringArray(vec!["original".to_string()]),
                    );
                    root.add_event("event", Some(event_attrs));
                    gate_cb.notified().await;
                    root.start_span(
                        SpanOptions::new("child"),
                        Arc::new(|_span| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
                    )
                    .await
                })
            }),
        )
        .await
    });

    // Yield once so `active` starts root span and pauses at `gate_cb.notified()`.
    tokio::task::yield_now().await;
    f.flush().await.expect("flush while open");
    assert_eq!(f.store().find_traces(None).await.expect("find_traces").traces, vec![]);
    assert_eq!(f.store().get_trace("missing").await.expect("get_trace"), None);

    gate.notify_one();
    active
        .await
        .expect("join active task")
        .expect("active span should resolve");
    f.flush().await.expect("flush after settle");

    let mut traces = f.store().find_traces(None).await.expect("find_traces").traces;
    let trace = traces.first_mut().expect("expected trace");
    let root_idx = trace
        .spans
        .iter()
        .position(|s| s.name == "root")
        .expect("expected root span");
    let child_idx = trace
        .spans
        .iter()
        .position(|s| s.name == "child")
        .expect("expected child span");

    let id_re = Regex::new(r"^[0-9a-f-]{36}:[1-9][0-9]*$").unwrap();
    let root_id = trace.spans[root_idx].id.clone();
    let root_trace_id = trace.spans[root_idx].trace_id.clone();
    assert!(id_re.is_match(&root_id), "unexpected root.id: {root_id}");
    assert_eq!(root_trace_id, root_id);
    assert_eq!(trace.spans[root_idx].parent_id, None);
    assert_eq!(trace.spans[child_idx].parent_id.as_deref(), Some(root_id.as_str()));
    assert_eq!(trace.spans[child_idx].trace_id, root_trace_id);
    assert_eq!(trace.spans[root_idx].started_ms, 20_000.0);
    assert!(trace.spans[root_idx].ended_ms >= trace.spans[root_idx].started_ms);
    assert!(trace.spans[root_idx].events[0].time_ms >= 0.0);

    trace.spans[root_idx].attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["changed".to_string()]),
    );
    trace.spans[root_idx].events[0].attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["changed".to_string()]),
    );

    let fresh = f
        .store()
        .get_trace(&trace.trace_id)
        .await
        .expect("get_trace")
        .expect("expected trace");
    let fresh_root = fresh
        .spans
        .iter()
        .find(|span| span.id == root_id)
        .expect("expected fresh root");
    assert_eq!(
        fresh_root.attributes.get("tags"),
        Some(&AttributeValue::StringArray(vec!["original".to_string()]))
    );
    assert_eq!(
        fresh_root.events[0].attributes.get("tags"),
        Some(&AttributeValue::StringArray(vec!["original".to_string()]))
    );

    f.async_dispose().await.expect("dispose");
}

pub async fn filters_by_first_span_name_and_start_any_error_and_all_attributes_on_one_span(
    factory: &StoreFixtureFactory,
) {
    let clock = Arc::new(MockClock::new(Some(10_000.0), None));
    let f = factory().await.expect("create fixture");

    let clock_old = Arc::clone(&clock);
    f.context()
        .start_span(
            SpanOptions::new("old"),
            Arc::new(move |root| {
                let clock_old = Arc::clone(&clock_old);
                Box::pin(async move {
                    clock_old.set_wall(30_000.0);
                    let mut left_attrs = SpanAttributes::new();
                    left_attrs.insert("left".to_string(), AttributeValue::Number(1.0));
                    root.start_span(
                        SpanOptions {
                            name: "recent-child".to_string(),
                            attributes: Some(left_attrs),
                            links: None,
                        },
                        Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
                    )
                    .await?;
                    let mut right_attrs = SpanAttributes::new();
                    right_attrs.insert("right".to_string(), AttributeValue::Number(2.0));
                    root.start_span(
                        SpanOptions {
                            name: "split".to_string(),
                            attributes: Some(right_attrs),
                            links: None,
                        },
                        Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
                    )
                    .await
                })
            }),
        )
        .await
        .expect("old span");

    clock.set_wall(20_000.0);
    let clock_wanted = Arc::clone(&clock);
    f.context()
        .start_span(
            SpanOptions::new("wanted"),
            Arc::new(move |root| {
                let clock_wanted = Arc::clone(&clock_wanted);
                Box::pin(async move {
                    clock_wanted.set_wall(20_001.0);
                    let mut attrs = SpanAttributes::new();
                    attrs.insert("left".to_string(), AttributeValue::Number(1.0));
                    attrs.insert("right".to_string(), AttributeValue::Number(2.0));
                    attrs.insert("valid".to_string(), AttributeValue::Boolean(true));
                    attrs.insert("odd.\"key".to_string(), AttributeValue::String("value".to_string()));
                    root.start_span(
                        SpanOptions {
                            name: "failure".to_string(),
                            attributes: Some(attrs),
                            links: None,
                        },
                        Arc::new(|child| {
                            Box::pin(async move {
                                child.set_status(SpanStatus::Error { error: None });
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                })
            }),
        )
        .await
        .expect("wanted span");

    clock.set_wall(40_000.0);
    let mut new_attrs = SpanAttributes::new();
    new_attrs.insert("valid".to_string(), AttributeValue::Number(1.0));
    f.context()
        .start_span(
            SpanOptions {
                name: "new".to_string(),
                attributes: Some(new_attrs),
                links: None,
            },
            Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
        )
        .await
        .expect("new span");

    f.flush().await.expect("flush");

    let names = |query: Option<TraceQuery>| {
        let store = f.store();
        async move {
            store
                .find_traces(query)
                .await
                .expect("find_traces")
                .traces
                .into_iter()
                .filter_map(|trace| trace.spans.into_iter().next().map(|s| s.name))
                .collect::<Vec<_>>()
        }
    };

    let mut left_right = SpanAttributes::new();
    left_right.insert("left".to_string(), AttributeValue::Number(1.0));
    left_right.insert("right".to_string(), AttributeValue::Number(2.0));

    assert_eq!(
        names(Some(TraceQuery {
            name: Some("wanted".to_string()),
            attributes: Some(left_right.clone()),
            errors: Some(true),
            start_min: Some(20_000.0),
            start_max: Some(20_000.0),
            ..Default::default()
        }))
        .await,
        vec!["wanted"]
    );
    assert_eq!(
        names(Some(TraceQuery {
            name: Some("failure".to_string()),
            ..Default::default()
        }))
        .await,
        Vec::<String>::new()
    );
    assert_eq!(
        names(Some(TraceQuery {
            attributes: Some(left_right),
            ..Default::default()
        }))
        .await,
        vec!["wanted"]
    );

    let mut odd_attrs = SpanAttributes::new();
    odd_attrs.insert("valid".to_string(), AttributeValue::Boolean(true));
    odd_attrs.insert("odd.\"key".to_string(), AttributeValue::String("value".to_string()));
    assert_eq!(
        names(Some(TraceQuery {
            attributes: Some(odd_attrs),
            ..Default::default()
        }))
        .await,
        vec!["wanted"]
    );

    let mut valid_num = SpanAttributes::new();
    valid_num.insert("valid".to_string(), AttributeValue::Number(1.0));
    assert_eq!(
        names(Some(TraceQuery {
            attributes: Some(valid_num),
            ..Default::default()
        }))
        .await,
        vec!["new"]
    );

    assert_eq!(
        names(Some(TraceQuery {
            errors: Some(true),
            ..Default::default()
        }))
        .await,
        vec!["wanted"]
    );
    assert_eq!(
        names(Some(TraceQuery {
            start_min: Some(20_000.0),
            ..Default::default()
        }))
        .await,
        vec!["new", "wanted"]
    );
    assert_eq!(
        names(Some(TraceQuery {
            start_max: Some(10_000.0),
            ..Default::default()
        }))
        .await,
        vec!["old"]
    );

    f.async_dispose().await.expect("dispose");
}

pub async fn pages_complete_traces_without_duplicates_when_trace_starts_tie(factory: &StoreFixtureFactory) {
    let clock = Arc::new(MockClock::new(Some(20_000.0), None));
    let f = factory().await.expect("create fixture");

    for index in 0..25 {
        clock.set_wall(20_000.0);
        let clock_cb = Arc::clone(&clock);
        let mut root_attrs = SpanAttributes::new();
        root_attrs.insert("index".to_string(), AttributeValue::Number(f64::from(index)));
        f.context()
            .start_span(
                SpanOptions {
                    name: "root".to_string(),
                    attributes: Some(root_attrs),
                    links: None,
                },
                Arc::new(move |root| {
                    let clock_cb = Arc::clone(&clock_cb);
                    Box::pin(async move {
                        clock_cb.set_wall(20_001.0);
                        root.start_span(
                            SpanOptions::new("child"),
                            Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
                        )
                        .await
                    })
                }),
            )
            .await
            .expect("root span");
    }

    f.flush().await.expect("flush");
    let first = f.store().find_traces(None).await.expect("first page");
    assert_eq!(first.traces.len(), 20);
    assert!(first.cursor.is_some());

    let rest = f
        .store()
        .find_traces(Some(TraceQuery {
            cursor: first.cursor.clone(),
            ..Default::default()
        }))
        .await
        .expect("rest page");
    assert_eq!(rest.traces.len(), 5);
    assert_eq!(rest.cursor, None);

    let all = f
        .store()
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            ..Default::default()
        }))
        .await
        .expect("all traces")
        .traces;
    let mut combined = first.traces;
    combined.extend(rest.traces);
    assert_eq!(combined, all);
    assert!(all.iter().all(|trace| trace.spans.len() == 2));

    let mut ids = Vec::new();
    let mut cursor = None;
    loop {
        let page = f
            .store()
            .find_traces(Some(TraceQuery {
                name: Some("root".to_string()),
                limit: Some(3),
                cursor,
                ..Default::default()
            }))
            .await
            .expect("paged traces");
        ids.extend(page.traces.into_iter().map(|t| t.trace_id));
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    let expected_ids: Vec<String> = all.into_iter().map(|t| t.trace_id).collect();
    assert_eq!(ids, expected_ids);

    f.async_dispose().await.expect("dispose");
}

pub async fn inherits_remote_trace_ids_and_preserves_remote_parents_and_cross_trace_links(
    factory: &StoreFixtureFactory,
) {
    let _clock = MockClock::new(Some(20_000.0), None);
    let sender = factory().await.expect("create sender");
    let receiver = factory().await.expect("create receiver");

    let carrier_holder: Arc<Mutex<Option<TelemetryCarrier>>> = Arc::new(Mutex::new(None));
    let carrier_cb = Arc::clone(&carrier_holder);
    let sender_prop = sender.propagation();
    sender
        .context()
        .start_span(
            SpanOptions::new("sender"),
            Arc::new(move |span| {
                let carrier_cb = Arc::clone(&carrier_cb);
                let sender_prop = Arc::clone(&sender_prop);
                Box::pin(async move {
                    *carrier_cb.lock().unwrap() = sender_prop.inject(&span);
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("sender span");
    sender.flush().await.expect("flush sender");

    let carrier = carrier_holder.lock().unwrap().clone().expect("expected carrier");
    let sent = sender.store().find_traces(None).await.expect("sent traces").traces[0].clone();

    let wire: TelemetryCarrier = pi_js::json::parse_as(&pi_js::json::stringify(&carrier)).expect("wire carrier");
    let remote = receiver.propagation().extract(Some(&wire));
    remote
        .start_span(
            SpanOptions::new("receiver"),
            Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
        )
        .await
        .expect("receiver span");

    let mut link_attrs = SpanAttributes::new();
    link_attrs.insert("reason".to_string(), AttributeValue::String("background".to_string()));
    receiver
        .context()
        .start_span(
            SpanOptions {
                name: "linked".to_string(),
                attributes: None,
                links: Some(vec![SpanLink {
                    context: remote,
                    attributes: Some(link_attrs.clone()),
                }]),
            },
            Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
        )
        .await
        .expect("linked span");
    receiver.flush().await.expect("flush receiver");

    let continued = receiver
        .store()
        .get_trace(&sent.trace_id)
        .await
        .expect("get continued trace")
        .expect("continued trace");
    assert_eq!(continued.spans.len(), 1);
    assert_eq!(continued.spans[0].trace_id, sent.trace_id);
    assert_eq!(continued.spans[0].parent_id.as_deref(), Some(sent.spans[0].id.as_str()));
    assert_eq!(continued.spans[0].name, "receiver");
    assert_ne!(continued.spans[0].id, sent.spans[0].id);

    let linked = receiver
        .store()
        .find_traces(Some(TraceQuery {
            name: Some("linked".to_string()),
            ..Default::default()
        }))
        .await
        .expect("linked trace")
        .traces[0]
        .clone();
    assert_ne!(linked.trace_id, sent.trace_id);
    assert_eq!(linked.spans[0].links.len(), 1);
    assert_eq!(linked.spans[0].links[0].trace_id, sent.trace_id);
    assert_eq!(linked.spans[0].links[0].span_id, sent.spans[0].id);
    assert_eq!(linked.spans[0].links[0].attributes.as_ref(), Some(&link_attrs));

    for key in carrier.keys() {
        let mut incomplete = carrier.clone();
        incomplete.shift_remove(key);
        receiver
            .propagation()
            .extract(Some(&incomplete))
            .start_span(
                SpanOptions::new("fallback"),
                Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
            )
            .await
            .expect("fallback incomplete");

        // PORT: `TelemetryCarrier` is strongly typed (`IndexMap<String, String>`); JSON deserialization rejects non-strings.
        let mut raw_json = serde_json::Map::new();
        for (k, v) in &carrier {
            if k == key {
                raw_json.insert(k.clone(), serde_json::Value::from(42));
            } else {
                raw_json.insert(k.clone(), serde_json::Value::from(v.as_str()));
            }
        }
        let parsed_non_string = pi_js::json::parse_as::<TelemetryCarrier>(&pi_js::json::stringify(&raw_json)).ok();
        receiver
            .propagation()
            .extract(parsed_non_string.as_ref())
            .start_span(
                SpanOptions::new("fallback"),
                Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
            )
            .await
            .expect("fallback non-string");
    }

    receiver.flush().await.expect("flush fallbacks");
    let fallbacks = receiver
        .store()
        .find_traces(Some(TraceQuery {
            name: Some("fallback".to_string()),
            ..Default::default()
        }))
        .await
        .expect("fallback traces")
        .traces;
    assert_eq!(fallbacks.len(), carrier.len() * 2);
    assert!(
        fallbacks
            .iter()
            .all(|trace| trace.spans[0].parent_id.is_none() && trace.trace_id == trace.spans[0].id)
    );

    receiver.async_dispose().await.expect("dispose receiver");
    sender.async_dispose().await.expect("dispose sender");
}

pub async fn accepts_external_string_identities_without_imposing_an_id_format(factory: &StoreFixtureFactory) {
    let f = factory().await.expect("create fixture");
    let carrier_holder: Arc<Mutex<Option<TelemetryCarrier>>> = Arc::new(Mutex::new(None));
    let carrier_cb = Arc::clone(&carrier_holder);
    let prop = f.propagation();
    f.context()
        .start_span(
            SpanOptions::new("source"),
            Arc::new(move |span| {
                let carrier_cb = Arc::clone(&carrier_cb);
                let prop = Arc::clone(&prop);
                Box::pin(async move {
                    *carrier_cb.lock().unwrap() = prop.inject(&span);
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("source span");

    let carrier = carrier_holder.lock().unwrap().clone().expect("carrier");
    let external: TelemetryCarrier = carrier
        .keys()
        .map(|key| (key.clone(), format!("external-{key}")))
        .collect();
    let parent = f.propagation().extract(Some(&external));
    assert_eq!(f.propagation().inject(&parent).as_ref(), Some(&external));

    parent
        .start_span(
            SpanOptions::new("continued"),
            Arc::new(|_s| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
        )
        .await
        .expect("continued span");
    f.flush().await.expect("flush");

    let trace = f
        .store()
        .find_traces(Some(TraceQuery {
            name: Some("continued".to_string()),
            ..Default::default()
        }))
        .await
        .expect("find continued")
        .traces[0]
        .clone();
    assert!(trace.trace_id.starts_with("external-"));
    assert!(
        trace.spans[0]
            .parent_id
            .as_deref()
            .is_some_and(|id| id.starts_with("external-"))
    );

    f.async_dispose().await.expect("dispose");
}

pub async fn queries_metric_names_inclusive_interval_bounds_typed_attribute_equality_and_detached_values(
    factory: &StoreFixtureFactory,
) {
    let clock = MockClock::new(Some(20_001.0), None);
    let f = factory().await.expect("create fixture");

    let counter = f
        .metrics()
        .counter("hits", Some(crate::store_conformance::instrument_with_unit("By")));
    let mut read_true = SpanAttributes::new();
    read_true.insert("method".to_string(), AttributeValue::String("read".to_string()));
    read_true.insert("valid".to_string(), AttributeValue::Boolean(true));
    counter.add(2.0, Some(read_true.clone()));

    let mut true_read = SpanAttributes::new();
    true_read.insert("valid".to_string(), AttributeValue::Boolean(true));
    true_read.insert("method".to_string(), AttributeValue::String("read".to_string()));
    counter.add(3.0, Some(true_read));

    let mut write_one = SpanAttributes::new();
    write_one.insert("method".to_string(), AttributeValue::String("write".to_string()));
    write_one.insert("valid".to_string(), AttributeValue::Number(1.0));
    counter.add(11.0, Some(write_one));

    f.metrics().counter("other", None).add(100.0, Some(read_true.clone()));

    clock.set_wall(30_001.0);
    counter.add(7.0, Some(read_true.clone()));
    clock.set_wall(40_001.0);
    f.flush().await.expect("flush");

    let query = MetricQuery {
        name: "hits".to_string(),
        start_min: Some(20_000),
        start_max: Some(20_000),
        attributes: Some(read_true),
    };
    let mut rows = f.store().find_metrics(query.clone()).await.expect("find_metrics");
    assert_eq!(rows.iter().map(|r| r.count).sum::<i64>(), 2);
    assert_eq!(rows.iter().map(|r| r.sum).sum::<f64>(), 5.0);
    assert_eq!(rows.iter().map(|r| r.min).fold(f64::INFINITY, f64::min), 2.0);
    assert_eq!(rows.iter().map(|r| r.max).fold(f64::NEG_INFINITY, f64::max), 3.0);
    assert!(
        rows.iter().all(|row| row.kind == MetricKind::Counter
            && row.unit.as_deref() == Some("By")
            && row.interval_ms == 20_000)
    );

    rows[0]
        .attributes
        .insert("method".to_string(), AttributeValue::String("changed".to_string()));
    assert_eq!(
        f.store().find_metrics(query).await.expect("find_metrics fresh")[0]
            .attributes
            .get("method"),
        Some(&AttributeValue::String("read".to_string()))
    );
    assert_eq!(
        f.store()
            .find_metrics(MetricQuery {
                name: "missing".to_string(),
                ..Default::default()
            })
            .await
            .expect("missing metrics"),
        vec![]
    );

    let mut valid_one = SpanAttributes::new();
    valid_one.insert("valid".to_string(), AttributeValue::Number(1.0));
    assert_eq!(
        f.store()
            .find_metrics(MetricQuery {
                name: "hits".to_string(),
                attributes: Some(valid_one),
                ..Default::default()
            })
            .await
            .expect("hits valid=1")
            .iter()
            .map(|r| r.sum)
            .sum::<f64>(),
        11.0
    );

    assert_eq!(
        f.store()
            .find_metrics(MetricQuery {
                name: "hits".to_string(),
                start_min: Some(30_000),
                ..Default::default()
            })
            .await
            .expect("hits startMin=30000")
            .iter()
            .map(|r| r.sum)
            .sum::<f64>(),
        7.0
    );

    f.async_dispose().await.expect("dispose");
}

fn instrument_with_unit(unit: &str) -> pi_telemetry::InstrumentOptions {
    pi_telemetry::InstrumentOptions {
        description: None,
        unit: Some(unit.to_string()),
    }
}

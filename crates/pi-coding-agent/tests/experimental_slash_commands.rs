//! Port of packages/coding-agent/test/experimental-slash-commands.test.ts

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::ServiceObject;
use pi_chord::{Facet, FacetOptions, create_facet_host, define_facet};
use pi_coding_agent::experimental::services::slash_commands::{
    SLASH_COMMANDS_SERVICE, SlashCommandContribution, SlashCommands, SlashCommandsApi,
};
use pi_coding_agent::experimental::services::slash_commands_provider::{
    SlashCommandRegistry, create_slash_commands_runtime_facet,
};

fn noop_run(
    counter: Option<Arc<AtomicUsize>>,
) -> pi_coding_agent::experimental::services::slash_commands::SlashCommandRunFn {
    Arc::new(move |_args, _ctx| {
        if let Some(ref c) = counter {
            c.fetch_add(1, Ordering::AcqRel);
        }
        Box::pin(futures::future::ready(Ok(None)))
    })
}

fn contribution(name: &str, description: &str, counter: Option<Arc<AtomicUsize>>) -> SlashCommandContribution {
    SlashCommandContribution {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint: None,
        get_argument_completions: None,
        run: noop_run(counter),
    }
}

#[test]
fn registers_and_removes_contributions() {
    let registry = SlashCommandRegistry::new();
    let snapshots: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshots_cb = Arc::clone(&snapshots);

    let unsubscribe = registry
        .subscribe(Arc::new(move |commands| {
            let names: Vec<String> = commands.iter().map(|c| c.name.clone()).collect();
            snapshots_cb.lock().expect("lock").push(names);
        }))
        .expect("subscribe");

    let close = registry
        .register(contribution("hello", "Hello", None))
        .expect("register hello");
    let names: Vec<String> = registry.list().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["hello"]);

    let dup_err = registry
        .register(contribution("hello", "", None))
        .err()
        .expect("duplicate must fail");
    assert!(dup_err.message().contains("already registered"));

    close.call();
    assert!(registry.list().is_empty());
    assert_eq!(
        *snapshots.lock().expect("lock"),
        vec![Vec::<String>::new(), vec!["hello".to_string()], Vec::new()]
    );
    unsubscribe.call();
}

#[test]
fn stages_replacements_until_the_previous_registration_retires() {
    let registry = SlashCommandRegistry::new();
    let first = contribution("hello", "First", None);
    let second = contribution("hello", "Second", None);

    let close_first = registry.register(first).expect("register first");
    let close_second = registry.replace(second.clone()).expect("replace second");
    let descs: Vec<String> = registry.list().into_iter().map(|c| c.description).collect();
    assert_eq!(descs, vec!["First"]);

    close_second.call();
    let descs: Vec<String> = registry.list().into_iter().map(|c| c.description).collect();
    assert_eq!(descs, vec!["First"]);

    let close_replacement = registry.replace(second).expect("replace again");
    close_first.call();
    let descs: Vec<String> = registry.list().into_iter().map(|c| c.description).collect();
    assert_eq!(descs, vec!["Second"]);

    close_replacement.call();
    assert!(registry.list().is_empty());
}

#[tokio::test]
async fn tracks_plugin_facet_reload_and_unload() {
    let registry = Arc::new(SlashCommandRegistry::new());
    let original_calls = Arc::new(AtomicUsize::new(0));
    let replacement_calls = Arc::new(AtomicUsize::new(0));

    let orig_cb = Arc::clone(&original_calls);
    let runtime_facet = create_slash_commands_runtime_facet(Some(Arc::clone(&registry)));
    let initial_facet = define_facet(Facet::new(
        "@test/example-hello",
        Arc::new(move |env| {
            let obj = env.r#use::<ServiceObject>(&*SLASH_COMMANDS_SERVICE)?;
            let commands = SlashCommands::from_service(obj);
            let orig_cb = Arc::clone(&orig_cb);
            let env_own = env.clone();
            env.on_activate(Arc::new(move || {
                let commands = commands.clone();
                let orig_cb = Arc::clone(&orig_cb);
                let env_own = env_own.clone();
                Box::pin(async move {
                    let unsub = commands.replace(contribution("hello", "Original", Some(orig_cb)))?;
                    let unsub = std::sync::Mutex::new(Some(unsub));
                    env_own.own(Arc::new(move || {
                        if let Some(u) = unsub.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            u.call();
                        }
                        Box::pin(futures::future::ready(Ok(())))
                    }))?;
                    Ok(())
                })
            }))
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![runtime_facet, initial_facet],
        service_sources: None,
        on_error: None,
    })
    .await
    .expect("create host");

    let names: Vec<String> = registry.list().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["hello"]);

    let failing_facet = define_facet(Facet::new(
        "@test/example-hello",
        Arc::new(|env| {
            let obj = env.r#use::<ServiceObject>(&*SLASH_COMMANDS_SERVICE)?;
            let commands = SlashCommands::from_service(obj);
            let env_own = env.clone();
            env.on_activate(Arc::new(move || {
                let commands = commands.clone();
                let env_own = env_own.clone();
                Box::pin(async move {
                    let unsub = commands.replace(contribution("hello", "Failing", None))?;
                    let unsub = std::sync::Mutex::new(Some(unsub));
                    env_own.own(Arc::new(move || {
                        if let Some(u) = unsub.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            u.call();
                        }
                        Box::pin(futures::future::ready(Ok(())))
                    }))?;
                    Err(pi_js::Error::msg("replacement failed"))
                })
            }))
        }),
    ));
    let reload_err = host
        .reload(vec![failing_facet])
        .await
        .err()
        .expect("reload should fail");
    assert!(reload_err.message().contains("replacement failed"));
    let list = registry.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].description, "Original");

    let repl_cb = Arc::clone(&replacement_calls);
    let replacement_facet = define_facet(Facet::new(
        "@test/example-hello",
        Arc::new(move |env| {
            let obj = env.r#use::<ServiceObject>(&*SLASH_COMMANDS_SERVICE)?;
            let commands = SlashCommands::from_service(obj);
            let repl_cb = Arc::clone(&repl_cb);
            let env_own = env.clone();
            env.on_activate(Arc::new(move || {
                let commands = commands.clone();
                let repl_cb = Arc::clone(&repl_cb);
                let env_own = env_own.clone();
                Box::pin(async move {
                    let unsub = commands.replace(contribution("hello", "Replacement", Some(repl_cb)))?;
                    let unsub = std::sync::Mutex::new(Some(unsub));
                    env_own.own(Arc::new(move || {
                        if let Some(u) = unsub.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            u.call();
                        }
                        Box::pin(futures::future::ready(Ok(())))
                    }))?;
                    Ok(())
                })
            }))
        }),
    ));

    host.reload(vec![replacement_facet]).await.expect("reload succeeds");
    let list = registry.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].description, "Replacement");
    let _ = (list[0].run)(String::new(), BACKGROUND_CONTEXT.clone()).await;
    assert_eq!(original_calls.load(Ordering::Acquire), 0);
    assert_eq!(replacement_calls.load(Ordering::Acquire), 1);

    host.dispose().await.expect("dispose");
    assert!(registry.list().is_empty());
}

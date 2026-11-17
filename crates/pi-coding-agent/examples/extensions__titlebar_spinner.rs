//! Port of packages/coding-agent/examples/extensions/titlebar-spinner.ts

use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    AgentSettledEvent, AgentStartEvent, Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension,
    SessionShutdownEvent,
};
use pi_js::time::{Interval, set_interval};

const BRAILLE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn get_base_title(pi: &ExtensionApi) -> String {
    let cwd = pi_js::path::basename(&pi_js::env::cwd(), None);
    let session = pi.get_session_name().ok().flatten();
    match session {
        Some(s) if !s.is_empty() => format!("π - {s} - {cwd}"),
        _ => format!("π - {cwd}"),
    }
}

fn stop_animation(
    pi: &ExtensionApi,
    timer: &Mutex<Option<Interval>>,
    frame_index: &AtomicUsize,
    ctx: &ExtensionContext,
) {
    let prev = timer.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(iv) = prev {
        iv.clear();
    }
    frame_index.store(0, Ordering::SeqCst);
    if let Ok(ui) = ctx.ui() {
        ui.set_title(&get_base_title(pi));
    }
}

fn start_animation(
    pi: &ExtensionApi,
    timer: &Mutex<Option<Interval>>,
    frame_index: &Arc<AtomicUsize>,
    ctx: &ExtensionContext,
) {
    stop_animation(pi, timer, frame_index, ctx);
    let pi_tick = pi.clone();
    let idx_tick = Arc::clone(frame_index);
    let ctx_tick = ctx.clone();
    let iv = set_interval(80, move || {
        let idx = idx_tick.fetch_add(1, Ordering::SeqCst);
        let frame = BRAILLE_FRAMES[idx % BRAILLE_FRAMES.len()];
        let cwd = pi_js::path::basename(&pi_js::env::cwd(), None);
        let session = pi_tick.get_session_name().ok().flatten();
        let title = match session {
            Some(s) if !s.is_empty() => format!("{frame} π - {s} - {cwd}"),
            _ => format!("{frame} π - {cwd}"),
        };
        if let Ok(ui) = ctx_tick.ui() {
            ui.set_title(&title);
        }
    });
    let old = timer.lock().unwrap_or_else(|e| e.into_inner()).replace(iv);
    if let Some(prev) = old {
        prev.clear();
    }
}

pub struct TitlebarSpinnerExtension;

#[async_trait]
impl Extension for TitlebarSpinnerExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let timer: Arc<Mutex<Option<Interval>>> = Arc::new(Mutex::new(None));
        let frame_index = Arc::new(AtomicUsize::new(0));

        {
            let pi_as = pi.clone();
            let t_as = Arc::clone(&timer);
            let f_as = Arc::clone(&frame_index);
            pi.on::<AgentStartEvent>(move |_event, ctx| {
                let pi = pi_as.clone();
                let timer = Arc::clone(&t_as);
                let frame_index = Arc::clone(&f_as);
                Box::pin(async move {
                    start_animation(&pi, &timer, &frame_index, &ctx);
                    Ok(None)
                })
            });
        }

        {
            let pi_st = pi.clone();
            let t_st = Arc::clone(&timer);
            let f_st = Arc::clone(&frame_index);
            pi.on::<AgentSettledEvent>(move |_event, ctx| {
                let pi = pi_st.clone();
                let timer = Arc::clone(&t_st);
                let frame_index = Arc::clone(&f_st);
                Box::pin(async move {
                    stop_animation(&pi, &timer, &frame_index, &ctx);
                    Ok(None)
                })
            });
        }

        {
            let pi_sh = pi.clone();
            let t_sh = Arc::clone(&timer);
            let f_sh = Arc::clone(&frame_index);
            pi.on::<SessionShutdownEvent>(move |_event, ctx| {
                let pi = pi_sh.clone();
                let timer = Arc::clone(&t_sh);
                let frame_index = Arc::clone(&f_sh);
                Box::pin(async move {
                    stop_animation(&pi, &timer, &frame_index, &ctx);
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(TitlebarSpinnerExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}

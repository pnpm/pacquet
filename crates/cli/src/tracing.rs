use std::{str::FromStr, sync::atomic::AtomicBool};

use tracing::Level;
use tracing_subscriber::{fmt::format::FmtSpan, layer::Filter, EnvFilter, Layer};

static IS_TRACING_ENABLED: AtomicBool = AtomicBool::new(false);

// skip event because it's not useful for performance analysis
struct FilterEvent;

impl<S> Filter<S> for FilterEvent {
    fn enabled(
        &self,
        meta: &tracing::Metadata<'_>,
        _cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        !meta.is_event()
    }
}
pub fn enable_tracing_by_env() {
    let trace_var = std::env::var("TRACE").ok();
    let is_enable_tracing = trace_var.is_some();

    if is_enable_tracing && !IS_TRACING_ENABLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        use tracing_subscriber::{fmt, prelude::*};
        let layers = generate_common_layers(trace_var);

        tracing_subscriber::registry()
            .with(layers)
            .with(fmt::layer().pretty().with_file(true).with_span_events(FmtSpan::CLOSE))
            .init();
        tracing::trace!("enable_tracing_by_env");
    }
}

fn generate_common_layers(
    trace_var: Option<String>,
) -> Vec<Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>> {
    let default_level = trace_var.as_ref().and_then(|var| Level::from_str(var).ok());

    let mut layers = vec![];
    if let Some(default_level) = default_level {
        layers.push(
            tracing_subscriber::filter::Targets::new()
                .with_targets(vec![("pacquet_tarball", default_level)])
                .boxed(),
        );
    } else {
        // SAFETY: we know that trace_var is `Ok(String)` now,
        // for the second unwrap, if we can't parse the directive, then the tracing result would be
        // unexpected, then panic is reasonable
        let env_layer = EnvFilter::builder()
      .with_regex(true)
      .parse(trace_var.expect("Should not be empty"))
      .expect("Parse tracing directive syntax failed,for details about the directive syntax you could refer https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives");

        layers.push(env_layer.boxed());
    }
    layers
}

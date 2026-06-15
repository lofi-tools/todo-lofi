use fastrace::collector::{Config, ConsoleReporter};
use fastrace::prelude::*;
use fastrace_tracing::FastraceCompatLayer;
use tracing_subscriber::layer::SubscriberExt;

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_tracing() {
    INIT.call_once(|| {
        fastrace::set_reporter(ConsoleReporter, Config::default());

        let subscriber = tracing_subscriber::Registry::default().with(FastraceCompatLayer::new());

        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

pub fn flush_tracing() {
    fastrace::flush();
}

pub fn start_root_span(name: String) -> Span {
    let root = Span::root(name, SpanContext::random());
    let _guard = root.set_local_parent();
    root
}

use fastrace::collector::{Config, ConsoleReporter};
use fastrace_tracing::FastraceCompatLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt};

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_tracing() {
    init_via_fastrace()
}

pub fn init_via_fastrace() {
    INIT.call_once(|| {
        fastrace::set_reporter(ConsoleReporter, Config::default());
        let subscriber = tracing_subscriber::Registry::default().with(FastraceCompatLayer::new());
        let _ = tracing::subscriber::set_global_default(subscriber);

        // let root = Span::root(name, SpanContext::random());
        // let _guard = root.set_local_parent();
        // root
    });
}

pub fn init_via_tracing_subscriber() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .init();
    });
}

pub fn flush_tracing() {
    fastrace::flush();
}

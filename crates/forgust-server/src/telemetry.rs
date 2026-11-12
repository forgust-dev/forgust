//! Logging, OpenTelemetry tracing and Prometheus metrics (ADR-0001).

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::TelemetryConfig;

pub struct Telemetry {
    pub metrics: Option<PrometheusHandle>,
    provider: Option<SdkTracerProvider>,
}

impl Telemetry {
    pub fn shutdown(&self) {
        if let Some(p) = &self.provider {
            let _ = p.shutdown();
        }
    }
}

/// Init logging only (CLI subcommands, hooks).
pub fn init_cli() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .try_init();
}

pub fn init(cfg: &TelemetryConfig) -> anyhow::Result<Telemetry> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_filter));
    let fmt = if cfg.log_format == "json" {
        tracing_subscriber::fmt::layer().json().with_current_span(false).boxed()
    } else {
        tracing_subscriber::fmt::layer().boxed()
    };

    let mut provider = None;
    let otel = if cfg.otlp_endpoint.is_empty() {
        None
    } else {
        let exporter = opentelemetry_otlp::SpanExporter::builder().with_tonic().with_endpoint(&cfg.otlp_endpoint).build()?;
        let p = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(Resource::builder().with_service_name("forgust").build())
            .build();
        let tracer = p.tracer("forgust");
        provider = Some(p);
        Some(tracing_opentelemetry::layer().with_tracer(tracer))
    };

    tracing_subscriber::registry().with(filter).with(fmt).with(otel).try_init()?;

    let metrics = if cfg.metrics {
        let handle =
            PrometheusBuilder::new().set_buckets(&[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0])?.install_recorder()?;
        Some(handle)
    } else {
        None
    };
    Ok(Telemetry { metrics, provider })
}

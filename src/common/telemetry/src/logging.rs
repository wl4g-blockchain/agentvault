// SPDX-License-Identifier: GNU GENERAL PUBLIC LICENSE Version 3
//
// Copyright (c) 2027 James Wong. This file is part of James Wong.
// is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// James Wong is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with James Wong.  If not, see <https://www.gnu.org/licenses/>.
//
// IMPORTANT: Any software that fully or partially contains or uses materials
// covered by this license must also be released under the GNU GPL license.
// This includes modifications and derived works.

use crate::tracing_sampler::create_sampler;

use crate::tracing_sampler::TracingSampleOptions;
use once_cell::sync::OnceCell;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::Sampler;
use opentelemetry_semantic_conventions::resource;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Once;
use std::time::Duration;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::RollingFileAppender;
use tracing_appender::rolling::Rotation;
use tracing_log::LogTracer;
use tracing_subscriber::filter;
use tracing_subscriber::filter::FilterFn;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Registry;

pub const DEFAULT_LOG_TARGETS: &str = "info";
pub const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4317";

// Handle for reloading log level
pub static RELOAD_HANDLE: OnceCell<tracing_subscriber::reload::Handle<Targets, Registry>> = OnceCell::new();

/// The logging options that used to initialize the logger.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingOptions {
    /// The directory to store log files. If not set, logs will be written to stdout.
    pub dir: String,

    /// The log level that can be one of "trace", "debug", "info", "warn", "error". Default is "info".
    pub level: Option<String>,

    /// The log format that can be one of "json" or "text". Default is "text".
    pub log_format: LogFormat,

    /// The maximum number of log files set by default.
    pub max_log_files: usize,

    /// Whether to append logs to stdout. Default is true.
    pub append_stdout: bool,

    /// Whether to enable tracing with OTLP. Default is false.
    pub enable_otlp_tracing: bool,

    /// The endpoint of OTLP. Default is "<http://localhost:4317>".
    pub otlp_endpoint: Option<String>,

    /// The tracing sample ratio.
    pub tracing_sample_ratio: Option<TracingSampleOptions>,

    /// The logging options of slow query.
    pub slow_query: SlowQueryOptions,
}

/// The options of slow query.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SlowQueryOptions {
    /// Whether to enable slow query log.
    pub enable: bool,

    /// The threshold of slow queries.
    #[serde(with = "humantime_serde")]
    pub threshold: Option<Duration>,

    /// The sample ratio of slow queries.
    pub sample_ratio: Option<f64>,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    Json,
    Text,
}

impl PartialEq for LoggingOptions {
    fn eq(&self, other: &Self) -> bool {
        self.dir == other.dir
            && self.level == other.level
            && self.enable_otlp_tracing == other.enable_otlp_tracing
            && self.otlp_endpoint == other.otlp_endpoint
            && self.tracing_sample_ratio == other.tracing_sample_ratio
            && self.append_stdout == other.append_stdout
    }
}

impl Eq for LoggingOptions {}

impl Default for LoggingOptions {
    fn default() -> Self {
        Self {
            dir: "./logs".to_string(),
            level: None,
            log_format: LogFormat::Text,
            enable_otlp_tracing: false,
            otlp_endpoint: None,
            tracing_sample_ratio: None,
            append_stdout: true,
            slow_query: SlowQueryOptions::default(),
            // Rotation hourly, 24 files per day, keeps info log files of 30 days
            max_log_files: 720,
        }
    }
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TracingOptions {
    #[cfg(feature = "profiling-tokio-console")]
    pub tokio_console_addr: Option<String>,
}

/// Installs the shared logging pipeline once and retains its writer guards.
///
/// # Panics
/// Panics when a log target cannot be opened, a filter/exporter is invalid, or
/// the process already installed an incompatible global subscriber.
#[allow(clippy::print_stdout)]
pub fn init_global_logging(
    app_name: &str,
    opts: &LoggingOptions,
    tracing_opts: &TracingOptions,
    node_id: Option<String>,
) -> Vec<WorkerGuard> {
    static START: Once = Once::new();
    let mut guards = vec![];

    START.call_once(|| {
        // Enable log compatible layer to convert log record to tracing span.
        LogTracer::init().expect("log tracer must be valid");

        let stdout_logging_layer = LogLayers::stdout(opts, &mut guards);
        let file_logging_layer = LogLayers::file(opts, "application", FileFilter::All, &mut guards);
        let err_file_logging_layer = LogLayers::file(opts, "application-err", FileFilter::Error, &mut guards);
        let slow_query_logging_layer = opts
            .slow_query
            .enable
            .then(|| LogLayers::file(opts, "application-slow-queries", FileFilter::Slow, &mut guards))
            .flatten();

        // The application configuration has already applied its YAML and
        // application-prefixed overrides before constructing these options.
        let filter = opts
            .level
            .as_deref()
            .unwrap_or(DEFAULT_LOG_TARGETS)
            .parse::<filter::Targets>()
            .expect("error parsing log level string");

        let (dyn_filter, reload_handle) = tracing_subscriber::reload::Layer::new(filter.clone());

        RELOAD_HANDLE
            .set(reload_handle)
            .expect("reload handle already set, maybe init_global_logging get called twice?");

        // Must enable 'tokio_unstable' cfg to use this feature.
        // For example: `RUSTFLAGS="--cfg tokio_unstable" cargo run -F common-telemetry/console -- standalone start`
        #[cfg(feature = "profiling-tokio-console")]
        let subscriber = {
            let tokio_console_layer = if let Some(tokio_console_addr) = &tracing_opts.tokio_console_addr {
                let addr: std::net::SocketAddr = tokio_console_addr.parse().unwrap_or_else(|e| {
                    panic!("Invalid binding address '{tokio_console_addr}' for tokio-console: {e}");
                });
                println!("tokio-console listening on {addr}");
                Some(console_subscriber::ConsoleLayer::builder().server_addr(addr).spawn())
            } else {
                None
            };

            Registry::default()
                .with(dyn_filter)
                .with(tokio_console_layer)
                .with(stdout_logging_layer)
                .with(file_logging_layer)
                .with(err_file_logging_layer)
                .with(slow_query_logging_layer)
        };

        // consume the `tracing_opts` to avoid "unused" warnings.
        let _ = tracing_opts;

        #[cfg(not(feature = "profiling-tokio-console"))]
        let subscriber = Registry::default()
            .with(dyn_filter)
            .with(stdout_logging_layer)
            .with(file_logging_layer)
            .with(err_file_logging_layer)
            .with(slow_query_logging_layer);

        if opts.enable_otlp_tracing {
            global::set_text_map_propagator(TraceContextPropagator::new());

            let provider = LogLayers::tracer_provider(app_name, opts, node_id);
            let tracer = provider.tracer(app_name.to_owned());
            global::set_tracer_provider(provider);

            tracing::subscriber::set_global_default(
                subscriber.with(tracing_opentelemetry::layer().with_tracer(tracer)),
            )
            .expect("error setting global tracing subscriber");
        } else {
            tracing::subscriber::set_global_default(subscriber).expect("error setting global tracing subscriber");
        }
    });

    guards
}

type BoxedLogLayer<S> = Box<dyn tracing_subscriber::Layer<S> + Send + Sync>;
#[derive(Clone, Copy)]
enum FileFilter {
    All,
    Error,
    Slow,
}
struct LogLayers;
impl LogLayers {
    fn formatted<S>(writer: tracing_appender::non_blocking::NonBlocking, json: bool, ansi: bool) -> BoxedLogLayer<S>
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        if json {
            Layer::new().json().with_writer(writer).with_ansi(ansi).boxed()
        } else {
            Layer::new().with_writer(writer).with_ansi(ansi).boxed()
        }
    }

    fn stdout<S>(opts: &LoggingOptions, guards: &mut Vec<WorkerGuard>) -> Option<BoxedLogLayer<S>>
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        if !opts.append_stdout {
            return None;
        }
        let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
        guards.push(guard);
        Some(Self::formatted(
            writer,
            opts.log_format == LogFormat::Json,
            atty::is(atty::Stream::Stdout),
        ))
    }

    fn file<S>(
        opts: &LoggingOptions,
        prefix: &str,
        filter: FileFilter,
        guards: &mut Vec<WorkerGuard>,
    ) -> Option<BoxedLogLayer<S>>
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        if opts.dir.is_empty() {
            return None;
        }
        let appender = RollingFileAppender::builder()
            .rotation(Rotation::HOURLY)
            .filename_prefix(prefix)
            .max_log_files(opts.max_log_files)
            .build(&opts.dir)
            .unwrap_or_else(|error| panic!("initializing rolling file appender at {} failed: {error}", opts.dir));
        let (writer, guard) = tracing_appender::non_blocking(appender);
        guards.push(guard);
        let layer = Self::formatted(writer, opts.log_format == LogFormat::Json, false);
        Some(match filter {
            FileFilter::All => layer,
            FileFilter::Error => layer.with_filter(filter::LevelFilter::ERROR).boxed(),
            FileFilter::Slow => layer
                .with_filter(FilterFn::new(|metadata| {
                    metadata.fields().iter().any(|field| field.name().contains("slow"))
                }))
                .boxed(),
        })
    }

    fn tracer_provider(
        app_name: &str,
        opts: &LoggingOptions,
        node_id: Option<String>,
    ) -> opentelemetry_sdk::trace::SdkTracerProvider {
        let sampler = opts
            .tracing_sample_ratio
            .as_ref()
            .map(create_sampler)
            .map_or(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)), Sampler::ParentBased);

        let resource = opentelemetry_sdk::Resource::builder()
            .with_attributes([
                KeyValue::new(resource::SERVICE_NAME.as_str().to_owned(), app_name.to_string()),
                KeyValue::new(
                    resource::SERVICE_INSTANCE_ID.as_str().to_owned(),
                    node_id.unwrap_or_else(|| "none".to_owned()),
                ),
                KeyValue::new(resource::SERVICE_VERSION.as_str().to_owned(), env!("CARGO_PKG_VERSION")),
                KeyValue::new(
                    resource::PROCESS_PID.as_str().to_owned(),
                    std::process::id().to_string(),
                ),
            ])
            .build();
        let endpoint = opts.otlp_endpoint.as_ref().map_or_else(
            || DEFAULT_OTLP_ENDPOINT.to_owned(),
            |endpoint| {
                if endpoint.starts_with("http") {
                    endpoint.clone()
                } else {
                    format!("http://{endpoint}")
                }
            },
        );
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .expect("otlp exporter construction failed");
        opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_resource(resource)
            .with_sampler(sampler)
            .with_batch_exporter(exporter)
            .build()
    }
}

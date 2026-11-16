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

//! tracing stuffs, inspired by `RisingWave`
use std::collections::HashMap;

use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::Tracer;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use std::sync::OnceLock;
use tracing_opentelemetry::OpenTelemetrySpanExt;

// An wapper for `Futures` that provides tracing instrument adapters.
pub trait FutureExt: std::future::Future + Sized {
    fn trace(self, span: tracing::span::Span) -> tracing::instrument::Instrumented<Self>;
}

impl<T: std::future::Future> FutureExt for T {
    #[inline]
    fn trace(self, span: tracing::span::Span) -> tracing::instrument::Instrumented<Self> {
        tracing::instrument::Instrument::instrument(self, span)
    }
}

/// Context for tracing used for propagating tracing information in a distributed system.
///
/// Generally, the caller of a service should create a tracing context from the current tracing span
/// and pass it to the callee through the network. The callee will then attach its local tracing
/// span as a child of the tracing context, so that the external tracing service can associate them
/// in a single trace.
///
/// The tracing context must be serialized into the W3C trace context format and passed in rpc
/// message headers when communication of frontend, datanode and meta.
///
/// See [Trace Context](https://www.w3.org/TR/trace-context/) for more information.
#[derive(Debug, Clone)]
pub struct TracingContext(opentelemetry::Context);

pub type W3cTrace = HashMap<String, String>;

/// An owned async-safe `OTel` span; no thread-local identity is installed.
pub struct MessageTrace {
    context: opentelemetry::Context,
}

impl MessageTrace {
    pub fn child(name: &'static str, traceparent: &str) -> Self {
        let fields = HashMap::from([("traceparent".to_owned(), traceparent.to_owned())]);
        let parent = TraceContextPropagator::new().extract(&fields);
        let span = opentelemetry::global::tracer("common.messaging").start_with_context(name, &parent);
        let mut context = parent.with_span(span);
        if !context.span().span_context().is_valid()
            || context.span().span_context().span_id() == parent.span().span_context().span_id()
        {
            // No exporter is required to maintain valid, real OTel span lifecycles.
            // A global NoopTracer can preserve the valid remote parent's context;
            // validity alone therefore does not prove that a child was created.
            static LOCAL: OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> = OnceLock::new();
            let tracer = LOCAL
                .get_or_init(|| opentelemetry_sdk::trace::SdkTracerProvider::builder().build())
                .tracer("common.messaging");
            context = parent.with_span(tracer.start_with_context(name, &parent));
        }
        Self { context }
    }

    #[must_use]
    /// Returns this span's W3C traceparent.
    ///
    /// # Panics
    /// Asserts that the tracer produced the valid context required by this span.
    pub fn traceparent(&self) -> String {
        let mut fields = HashMap::new();
        TraceContextPropagator::new().inject_context(&self.context, &mut fields);
        fields
            .remove("traceparent")
            .expect("a valid OTel span always has traceparent")
    }
}

impl Drop for MessageTrace {
    fn drop(&mut self) {
        self.context.span().end();
    }
}

impl Default for TracingContext {
    fn default() -> Self {
        Self::new()
    }
}

type Propagator = TraceContextPropagator;

impl TracingContext {
    /// Create a new tracing context from a tracing span.
    #[must_use]
    pub fn from_span(span: &tracing::Span) -> Self {
        Self(span.context())
    }

    /// Create a new tracing context from the current tracing span considered by the subscriber.
    #[must_use]
    pub fn from_current_span() -> Self {
        Self::from_span(&tracing::Span::current())
    }

    /// Create a no-op tracing context.
    #[must_use]
    pub fn new() -> Self {
        Self(opentelemetry::Context::new())
    }

    /// Attach the given span as a child of the context. Returns the attached span.
    #[must_use]
    pub fn attach(&self, span: tracing::Span) -> tracing::Span {
        let _ = span.set_parent(self.0.clone());
        span
    }

    /// Injects the active span through the application-installed propagator.
    pub fn inject_current(injector: &mut dyn opentelemetry::propagation::Injector) {
        let context = tracing::Span::current().context();
        opentelemetry::global::get_text_map_propagator(|propagator| propagator.inject_context(&context, injector));
    }

    /// Convert the tracing context to the W3C trace context format.
    #[must_use]
    pub fn to_w3c(&self) -> W3cTrace {
        let mut fields = HashMap::new();
        Propagator::new().inject_context(&self.0, &mut fields);
        fields
    }

    /// Create a new tracing context from the W3C trace context format.
    #[must_use]
    pub fn from_w3c(fields: &W3cTrace) -> Self {
        let context = Propagator::new().extract(fields);
        Self(context)
    }

    /// Convert the tracing context to a JSON string in W3C trace context format.
    #[must_use]
    /// # Panics
    /// Asserts the invariant that the string-only W3C field map always serializes.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.to_w3c()).unwrap()
    }

    /// Create a new tracing context from a JSON string in W3C trace context format.
    ///
    /// Illegal json string will produce an empty tracing context and no error will be reported.
    #[must_use]
    pub fn from_json(json: &str) -> Self {
        let fields: W3cTrace = serde_json::from_str(json).unwrap_or_default();
        Self::from_w3c(&fields)
    }
}

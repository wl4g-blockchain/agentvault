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

use opentelemetry::trace::Link;

use opentelemetry_sdk::trace::SamplingDecision;

use opentelemetry_sdk::trace::SamplingResult;

use opentelemetry::trace::SpanKind;

use opentelemetry::trace::TraceContextExt;

use opentelemetry::trace::TraceId;

use opentelemetry::trace::TraceState;
use opentelemetry::KeyValue;
use opentelemetry_sdk::trace::Sampler;
use opentelemetry_sdk::trace::ShouldSample;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TracingSampleOptions {
    pub default_ratio: f64,
    pub rules: Vec<TracingSampleRule>,
}

impl Default for TracingSampleOptions {
    fn default() -> Self {
        Self {
            default_ratio: 1.0,
            rules: vec![],
        }
    }
}

/// Determine the sampling rate of a span according to the `rules` provided in `RuleSampler`.
/// For spans that do not hit any `rules`, the `default_ratio` is used.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TracingSampleRule {
    pub protocol: String,
    pub request_types: HashSet<String>,
    pub ratio: f64,
}

impl TracingSampleRule {
    pub fn match_rule(&self, protocol: &str, request_type: Option<&str>) -> Option<f64> {
        if protocol == self.protocol {
            if self.request_types.is_empty() {
                Some(self.ratio)
            } else {
                match request_type {
                    Some(t) if self.request_types.contains(t) => Some(self.ratio),
                    _ => None,
                }
            }
        } else {
            None
        }
    }
}

impl PartialEq for TracingSampleOptions {
    fn eq(&self, other: &Self) -> bool {
        self.default_ratio == other.default_ratio && self.rules == other.rules
    }
}
impl PartialEq for TracingSampleRule {
    fn eq(&self, other: &Self) -> bool {
        self.protocol == other.protocol && self.request_types == other.request_types && self.ratio == other.ratio
    }
}

impl Eq for TracingSampleOptions {}
impl Eq for TracingSampleRule {}

pub fn create_sampler(opt: &TracingSampleOptions) -> Box<dyn ShouldSample> {
    if opt.rules.is_empty() {
        Box::new(Sampler::TraceIdRatioBased(opt.default_ratio))
    } else {
        Box::new(opt.clone())
    }
}

impl ShouldSample for TracingSampleOptions {
    fn should_sample(
        &self,
        parent_context: Option<&opentelemetry::Context>,
        trace_id: TraceId,
        _name: &str,
        _span_kind: &SpanKind,
        attributes: &[KeyValue],
        _links: &[Link],
    ) -> SamplingResult {
        let (mut protocol, mut request_type) = (None, None);
        for kv in attributes {
            match kv.key.as_str() {
                "protocol" => protocol = Some(kv.value.as_str()),
                "request_type" => request_type = Some(kv.value.as_str()),
                _ => (),
            }
        }
        let ratio = protocol
            .and_then(|p| {
                self.rules
                    .iter()
                    .find_map(|rule| rule.match_rule(p.as_ref(), request_type.as_deref()))
            })
            .unwrap_or(self.default_ratio);
        SamplingResult {
            decision: sample_based_on_probability(ratio, trace_id),
            // No extra attributes ever set by the SDK samplers.
            attributes: Vec::new(),
            // all sampler in SDK will not modify trace state.
            trace_state: match parent_context {
                Some(ctx) => ctx.span().span_context().trace_state().clone(),
                None => TraceState::default(),
            },
        }
    }
}

/// The code here mainly refers to the relevant implementation of
/// [opentelemetry](https://github.com/open-telemetry/opentelemetry-rust/blob/ef4701055cc39d3448d5e5392812ded00cdd4476/opentelemetry-sdk/src/trace/sampler.rs#L229),
/// and determines whether the span needs to be collected based on the `TraceId` and sampling rate (i.e. `prob`).
fn sample_based_on_probability(prob: f64, trace_id: TraceId) -> SamplingDecision {
    if prob >= 1.0 {
        SamplingDecision::RecordAndSample
    } else {
        // Preserve the upstream sampler's intentional nonnegative threshold truncation.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let prob_upper_bound = (prob.max(0.0) * (1u64 << 63) as f64) as u64;
        let bytes = trace_id.to_bytes();
        let (_, low) = bytes.split_at(8);
        let trace_id_low = u64::from_be_bytes(low.try_into().unwrap());
        let rnd_from_trace_id = trace_id_low >> 1;

        if rnd_from_trace_id < prob_upper_bound {
            SamplingDecision::RecordAndSample
        } else {
            SamplingDecision::Drop
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/tracing_sampler_test.rs"]
mod test;

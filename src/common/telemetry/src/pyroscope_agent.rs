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

use crate::info;

#[derive(Debug, Clone)]
pub struct PyroscopeAgentOptions {
    pub enabled: bool,
    pub service_name: String,
    pub server_url: String,
    pub auth_token: Option<String>,
    pub tags: Option<Vec<(String, String)>>,
    pub sample_rate: f32,
}

pub async fn init_profiling(options: &PyroscopeAgentOptions) {
    let agent = if options.enabled && options.enabled {
        let mut tags = Vec::new();
        tags.push(("role", "primary"));
        //tags.push(("instance", &sigbot_utils::inets::get_local_non_loopback_ip_str()));
        // Merge tags with configuration.
        if let Some(ctags) = &options.tags {
            for (key, value) in ctags {
                tags.push((key.as_str(), value.as_str()));
            }
        }
        // Create pyroscope agent.
        // https://grafana.com/docs/pyroscope/latest/configure-client/language-sdks/rust/
        let mut builder = pyroscope::PyroscopeAgent::builder(&options.server_url, &options.service_name)
            .tags(tags)
            .backend(pyroscope_pprofrs::pprof_backend(
                pyroscope_pprofrs::PprofConfig::new().sample_rate(100),
            ));
        builder = match &options.auth_token {
            Some(token) => builder.auth_token(token),
            None => builder,
        };
        Some(builder.build().expect("Failed to setup pyroscope agent"))
    } else {
        None
    };
    if agent.is_some() {
        info!("Pyroscope agent profiling starting ...");
        agent.unwrap().start().expect("Failed to start pyroscope agent");
    }
}

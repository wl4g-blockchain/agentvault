// Copyright 2023 SigBot Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::error::BuildTempPathSnafu;
use crate::error::DumpProfileDataSnafu;
use crate::error::OpenTempFileSnafu;
use crate::error::ProfilingNotEnabledSnafu;
use crate::error::ReadOptProfSnafu;
use crate::error::Result;
use snafu::ensure;
use snafu::ResultExt;
use std::ffi::c_char;
use std::ffi::CString;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

const PROF_DUMP: &[u8] = b"prof.dump\0";
const OPT_PROF: &[u8] = b"opt.prof\0";

/// Dumps the allocator profile without exposing allocator-owned pointers.
///
/// # Errors
/// Reports disabled profiling, allocator-control failures, invalid temporary paths,
/// and temporary profile-file creation or read failures.
#[allow(unsafe_code)]
pub async fn dump_profile() -> Result<Vec<u8>> {
    ensure!(is_prof_enabled()?, ProfilingNotEnabledSnafu);

    let tmp_path = tempfile::tempdir().map_err(|_| {
        BuildTempPathSnafu {
            path: std::env::temp_dir(),
        }
        .build()
    })?;
    let mut path_buf = PathBuf::from(tmp_path.path());
    path_buf.push("heap.hprof");

    let path = path_buf
        .to_str()
        .ok_or_else(|| BuildTempPathSnafu { path: &path_buf }.build())?
        .to_string();

    let mut bytes = CString::new(path.as_str())
        .map_err(|_| BuildTempPathSnafu { path: &path_buf }.build())?
        .into_bytes_with_nul();

    {
        // #safety: we always expect a valid temp file path to write profiling data to.
        let ptr = bytes.as_mut_ptr().cast::<c_char>();
        unsafe { tikv_jemalloc_ctl::raw::write(PROF_DUMP, ptr).context(DumpProfileDataSnafu { path: path_buf })? }
    }

    let mut f = tokio::fs::File::open(path.as_str())
        .await
        .context(OpenTempFileSnafu { path: &path })?;
    let mut buf = vec![];
    let _ = f.read_to_end(&mut buf).await.context(OpenTempFileSnafu { path })?;
    Ok(buf)
}

#[allow(unsafe_code)]
fn is_prof_enabled() -> Result<bool> {
    // safety: OPT_PROF variable, if present, is always a boolean value.
    Ok(unsafe { tikv_jemalloc_ctl::raw::read::<bool>(OPT_PROF).context(ReadOptProfSnafu)? })
}

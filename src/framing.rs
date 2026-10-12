use tokio::io::{AsyncBufRead, AsyncBufReadExt as _};

use crate::{Error, Result};

/// Reads one newline-delimited frame without allocating beyond the configured limit.
pub(crate) async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
    description: &'static str,
) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(1024.min(limit));
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(output);
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if output.len() + consumed > limit {
            return Err(Error::InvalidRequest(format!(
                "{description} exceeds configured byte limit"
            )));
        }
        output.extend_from_slice(&available[..consumed]);
        let complete = available.get(consumed.saturating_sub(1)) == Some(&b'\n');
        reader.consume(consumed);
        if complete {
            output.pop();
            return Ok(output);
        }
    }
}

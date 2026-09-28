use crate::prelude::*;

pub(crate) fn validate_rs_counts(data_shards: usize, parity_shards: usize) -> Result<()> {
    if data_shards == 0 {
        bail!("data-shards must be greater than zero");
    }
    if parity_shards == 0 {
        bail!("parity-shards must be greater than zero when Reed-Solomon coding is enabled");
    }
    if data_shards + parity_shards > 255 {
        bail!("Reed-Solomon GF(256) requires data-shards + parity-shards <= 255");
    }
    Ok(())
}

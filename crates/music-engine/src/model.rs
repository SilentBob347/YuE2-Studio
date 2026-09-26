//! What the adapter library asks of YuE2: the parts of the model an
//! adapter changes, the sizes it comes in and what an adapter file holds.

pub use crate::yue_server::{AdapterSlot, ADAPTER_SLOTS};
pub use music_core::adapter_weights::AdapterWeights;
use music_core::adapter_weights::{adapter_format, tensor_names};
use serde_json::Value;

/// YuE2 comes in one size, so an adapter's width says nothing to
/// choose by.
pub const MODEL_FAMILIES: &[(&str, u64)] = &[];

/// The size a model file holds; one size, none to name.
pub fn model_family(_model_file: &str) -> Option<&'static str> {
    None
}

/// yue2.cpp merges LoRA and LoKr and refuses DoRA in any form (its adapter.h).
pub fn describe_adapter(header: &serde_json::Map<String, Value>) -> AdapterWeights {
    if tensor_names(header).iter().any(|name| name.contains(".dora_scale")) {
        return AdapterWeights::refused("DoRA");
    }
    match adapter_format(header) {
        Ok(format) => AdapterWeights { format: Some(format.into()), ..AdapterWeights::default() },
        Err(problem) => AdapterWeights::refused(problem),
    }
}

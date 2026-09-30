//! Where a local model sits: on the GPU, on the CPU, or split between them

use serde::Serialize;

/// A model Ollama has loaded, as its `/api/ps` reports it
///
/// `/api/ps` says where the model's memory is and when it unloads. It has no
/// utilization, so a model on the GPU that is busy with something else looks
/// the same as an idle one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelSeat {
    /// The model's name
    pub name: String,
    /// Bytes of memory the loaded model takes, on the GPU and the CPU
    pub size: u64,
    /// Bytes of that on the GPU
    pub size_vram: u64,
    /// The context length it was loaded with, in tokens
    pub context_length: Option<u64>,
    /// When Ollama unloads it, as it wrote the time
    pub expires_at: Option<String>,
}

impl ModelSeat {
    /// The share of the model on the GPU, in whole percent, rounded down
    pub fn gpu_percent(&self) -> u64 {
        match self.size {
            0 => 0,
            size => u64::try_from(u128::from(self.size_vram.min(size)) * 100 / u128::from(size))
                .unwrap_or(100),
        }
    }

    /// Whether any of the model is on the CPU
    pub fn spilled(&self) -> bool {
        self.size_vram < self.size
    }

    /// Why a round does not run against this model, when it is spilled
    pub fn spill(&self) -> Option<String> {
        self.spilled().then(|| {
            format!(
                "the local model {} is {}% on the GPU, so its rounds would run at CPU speed",
                self.name,
                self.gpu_percent()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(size: u64, size_vram: u64) -> ModelSeat {
        ModelSeat {
            name: "coder".into(),
            size,
            size_vram,
            context_length: None,
            expires_at: None,
        }
    }

    #[test]
    fn a_model_is_spilled_when_any_of_it_is_off_the_gpu() {
        assert!(!seat(100, 100).spilled());
        assert!(seat(100, 99).spilled());
        assert!(seat(100, 0).spilled());
        assert_eq!(seat(100, 99).gpu_percent(), 99);
        assert_eq!(seat(100, 0).gpu_percent(), 0);
        assert_eq!(seat(0, 0).gpu_percent(), 0);
        assert_eq!(seat(100, 100).spill(), None);
        assert_eq!(
            seat(100, 25).spill().as_deref(),
            Some(
                "the local model coder is 25% on the GPU, \
                 so its rounds would run at CPU speed"
            )
        );
    }
}

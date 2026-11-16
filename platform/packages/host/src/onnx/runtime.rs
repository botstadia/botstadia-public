//! A seat's models on ONNX Runtime.
//!
//! Each seat loads its own sessions: one thread, the graph optimisation level
//! fixed, the CPU memory arena and memory pattern planning off, denormals
//! flushed to zero. Inputs and outputs are matched by name.

use anyhow::{Context, Result, anyhow};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use super::preflight::ModelInfo;

/// One tensor, row-major, as the `agent` world passes them.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NamedTensor {
    pub name: String,
    pub shape: Vec<u32>,
    pub data: Vec<f32>,
}

/// Stops a run from another thread: ONNX Runtime's run termination
/// (BOTS.md: a call that trips the backstop is cancelled, and returns an
/// error).
#[derive(Clone)]
pub struct RunControl(std::sync::Arc<ort::session::RunOptions>);

impl RunControl {
    pub fn new() -> Result<Self> {
        Ok(Self(std::sync::Arc::new(
            ort::session::RunOptions::new().map_err(|error| anyhow!("{error}"))?,
        )))
    }

    /// Stop the run in flight, and any that starts before [`Self::reset`].
    pub fn terminate(&self) {
        let _ = self.0.terminate();
    }

    /// Let runs go again.
    pub fn reset(&self) {
        let _ = self.0.unterminate();
    }
}

/// A loaded model and what preflight found about it.
pub struct LoadedModel {
    pub name: String,
    pub info: ModelInfo,
    session: Session,
}

impl LoadedModel {
    pub fn load(name: &str, bytes: &[u8], info: ModelInfo) -> Result<Self> {
        let session = Session::builder()
            .map_err(|error| anyhow!("{error}"))?
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(|error| anyhow!("{error}"))?
            .with_intra_threads(1)
            .map_err(|error| anyhow!("{error}"))?
            .with_inter_threads(1)
            .map_err(|error| anyhow!("{error}"))?
            .with_memory_pattern(false)
            .map_err(|error| anyhow!("{error}"))?
            .with_flush_to_zero()
            .map_err(|error| anyhow!("{error}"))?
            .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
            .map_err(|error| anyhow!("{error}"))?
            .commit_from_memory(bytes)
            .map_err(|error| anyhow!("loading model `{name}`: {error}"))?;
        Ok(Self {
            name: name.into(),
            info,
            session,
        })
    }

    /// Run the model. Every model input must be given, by name, with its
    /// exact shape; outputs come back in the model's order.
    pub fn run(&mut self, inputs: &[NamedTensor]) -> Result<Vec<NamedTensor>, String> {
        self.run_with(inputs, None)
    }

    /// [`Self::run`], stoppable through `control`.
    pub fn run_with(
        &mut self,
        inputs: &[NamedTensor],
        control: Option<&RunControl>,
    ) -> Result<Vec<NamedTensor>, String> {
        let mut feed = Vec::with_capacity(self.info.inputs.len());
        for want in &self.info.inputs {
            let Some(given) = inputs.iter().find(|tensor| tensor.name == want.name) else {
                return Err(format!("no input named `{}`", want.name));
            };
            let shape: Vec<i64> = given.shape.iter().map(|d| i64::from(*d)).collect();
            if shape != want.shape {
                return Err(format!(
                    "input `{}` is {:?}, the model takes {:?}",
                    want.name, shape, want.shape
                ));
            }
            if given.data.len() as u64 != want.elements() {
                return Err(format!(
                    "input `{}` has {} values for shape {:?}",
                    want.name,
                    given.data.len(),
                    want.shape
                ));
            }
            let tensor = Tensor::from_array((shape, given.data.clone()))
                .map_err(|error| format!("input `{}`: {error}", want.name))?;
            feed.push((want.name.clone(), tensor.into_dyn()));
        }
        let outputs = match control {
            Some(control) => self.session.run_with_options(feed, &control.0),
            None => self.session.run(feed),
        }
        .map_err(|error| format!("inference failed: {error}"))?;
        let mut result = Vec::with_capacity(self.info.outputs.len());
        for want in &self.info.outputs {
            let value = outputs
                .get(want.name.as_str())
                .ok_or_else(|| format!("no output `{}`", want.name))?;
            let (shape, data) = value
                .try_extract_tensor::<f32>()
                .map_err(|error| format!("output `{}`: {error}", want.name))?;
            result.push(NamedTensor {
                name: want.name.clone(),
                shape: shape.iter().map(|d| *d as u32).collect(),
                data: data.to_vec(),
            });
        }
        Ok(result)
    }
}

/// Start ONNX Runtime's environment now, before a process's syscall filter
/// goes on, so nothing it sets up later needs more than the filter allows.
pub fn start_environment() -> Result<()> {
    Session::builder().map_err(|error| anyhow!("{error}"))?;
    Ok(())
}

/// Load every model in a bundle, preflighted, in manifest order.
pub fn load_all(models: &[(String, Vec<u8>)], infos: &[ModelInfo]) -> Result<Vec<LoadedModel>> {
    models
        .iter()
        .zip(infos)
        .map(|((name, bytes), info)| {
            LoadedModel::load(name, bytes, info.clone()).with_context(|| format!("model `{name}`"))
        })
        .collect()
}

//! A seat's ONNX Runtime process: loads the models its seat's Wasmtime
//! process sent (through the runner) and runs calls one at a time. A reader
//! thread, started before the filter, takes the runner's cancel at any
//! moment and stops the call in flight with ONNX Runtime's run termination.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;

use botstadia_host::onnx::runtime::{LoadedModel, NamedTensor, RunControl};
use botstadia_host::seat::InferFailure;

use super::frame::{self, Frame, MAX_FRAME, decode, encode};
use super::*;

pub fn run(
    _budget: u32,
    mut r: Reader,
    mut w: Writer,
    filter: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    botstadia_host::onnx::runtime::start_environment()?;
    let control = RunControl::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel::<Frame>();
    {
        let (control, cancelled) = (control.clone(), cancelled.clone());
        std::thread::Builder::new()
            .name("inference reader".into())
            .spawn(move || {
                while let Ok(Some(f)) = frame::read(&mut r) {
                    if f.kind == INF_CANCEL {
                        cancelled.store(true, Ordering::SeqCst);
                        control.terminate();
                    } else if tx.send(f).is_err() {
                        return;
                    }
                }
            })?;
    }
    filter()?;
    let mut models: Vec<LoadedModel> = Vec::new();
    for f in rx {
        match f.kind {
            INF_LOAD => {
                // The encoding goes once decoded, and each model's bytes once
                // it's loaded, so the load never holds more than its cap.
                let list = decode::<Vec<ModelToLoad>>(&f.body, MAX_FRAME as u64);
                drop(f);
                let loaded = list.and_then(|list| {
                    list.into_iter()
                        .map(|m| LoadedModel::load(&m.name, &m.bytes, m.info))
                        .collect::<anyhow::Result<Vec<_>>>()
                });
                let answer: Result<(), String> = match loaded {
                    Ok(list) => {
                        models = list;
                        Ok(())
                    }
                    Err(e) => Err(format!("{e:#}")),
                };
                frame::write(&mut w, &Frame::new(INF_LOADED, 0, encode(&answer)))?;
            }
            INF_RUN => {
                cancelled.store(false, Ordering::SeqCst);
                control.reset();
                let result: Result<Vec<NamedTensor>, InferFailure> =
                    match decode::<(u32, Vec<NamedTensor>)>(&f.body, MAX_FRAME as u64) {
                        Err(e) => Err(InferFailure::BadInput(e.to_string())),
                        Ok((model, inputs)) => match models.get_mut(model as usize) {
                            None => Err(InferFailure::BadInput(format!(
                                "the bundle has no model {model}"
                            ))),
                            Some(m) => m.run_with(&inputs, Some(&control)).map_err(|e| {
                                if cancelled.load(Ordering::SeqCst) {
                                    InferFailure::Stopped
                                } else {
                                    InferFailure::BadInput(e)
                                }
                            }),
                        },
                    };
                frame::write(&mut w, &Frame::new(INF_OUTPUTS, 0, encode(&result)))?;
            }
            _ => anyhow::bail!("an inference process takes loads, runs and cancels"),
        }
    }
    Ok(())
}

//! The minimal awsm-audio worklet: a stereo gain (each channel × the `gain`
//! param). This is the reference an agent follows when authoring a worklet over
//! MCP (see the `awsm://docs/worklet-abi` resource): implement [`Processor`] and
//! call [`awsm_worklet!`] once, then build to `wasm32-unknown-unknown`.
//!
//! ```sh
//! cargo build -p awsm-audio-worklet-gain --target wasm32-unknown-unknown --release
//! ```

use awsm_audio_worklet::{awsm_worklet, ParamDesc, Params, Processor};

struct Gain;

impl Processor for Gain {
    const PARAMS: &'static [ParamDesc] = &[ParamDesc::new("gain", 0.0, 2.0, 1.0)];

    fn new(_sample_rate: f32) -> Self {
        Gain
    }

    fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], params: &Params) {
        let g = params.get(0);
        for ch in 0..output.len() {
            let inp = input[ch];
            for (o, &i) in output[ch].iter_mut().zip(inp) {
                *o = i * g;
            }
        }
    }
}

awsm_worklet!(Gain);

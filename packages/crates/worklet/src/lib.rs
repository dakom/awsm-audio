//! Write a WASM DSP processor for the awsm-audio editor's **AudioWorklet** node.
//!
//! Implement [`Processor`] for your type and invoke [`awsm_worklet!`] once; the
//! macro generates all the exports the editor's generic worklet shim expects
//! (the awsm-audio worklet ABI). Compile your crate as a `cdylib` to
//! `wasm32-unknown-unknown` and load the resulting `.wasm` into an AudioWorklet
//! node — its [`PARAMS`](Processor::PARAMS) are auto-discovered and become
//! editable, automatable, modulation-targetable knobs.
//!
//! Processing is **mono**: each render quantum you get an input slice and write
//! an output slice of the same length (≤ [`MAX_FRAMES`]).
//!
//! ```ignore
//! use awsm_audio_worklet::*;
//!
//! struct Gain;
//! impl Processor for Gain {
//!     const PARAMS: &'static [ParamDesc] = &[ParamDesc::new("gain", 0.0, 2.0, 1.0)];
//!     fn new(_sample_rate: f32) -> Self { Gain }
//!     fn process(&mut self, input: &[f32], output: &mut [f32], params: &Params) {
//!         let g = params.get(0);
//!         for (o, &i) in output.iter_mut().zip(input) { *o = i * g; }
//!     }
//! }
//! awsm_worklet!(Gain);
//! ```
#![no_std]

/// Maximum frames per render quantum (WebAudio renders 128).
pub const MAX_FRAMES: usize = 128;
/// Maximum number of parameters a processor may declare (the shim's bank size).
pub const MAX_PARAMS: usize = 32;
/// Channel count the ABI processes (stereo). Mono input is duplicated to both.
pub const CHANNELS: usize = 2;

/// A parameter descriptor: name + display range + default, read by the editor to
/// build a labelled, ranged control.
pub struct ParamDesc {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

impl ParamDesc {
    pub const fn new(name: &'static str, min: f32, max: f32, default: f32) -> Self {
        Self {
            name,
            min,
            max,
            default,
        }
    }
}

/// The current parameter values for a render quantum (one per declared param,
/// in [`Processor::PARAMS`] order).
pub struct Params<'a> {
    pub values: &'a [f32],
}

impl Params<'_> {
    /// Value of param `i` (0 if out of range).
    #[inline]
    pub fn get(&self, i: usize) -> f32 {
        self.values.get(i).copied().unwrap_or(0.0)
    }
}

/// Small `no_std` DSP math approximations (so processors stay import-free —
/// `f32::sin`/`tanh` pull in extra symbols on `wasm32-unknown-unknown`).
pub mod math {
    /// π.
    pub const PI: f32 = core::f32::consts::PI;
    /// 2π.
    pub const TAU: f32 = core::f32::consts::TAU;

    /// Fast sine approximation (radians). Wraps the input to `[-π, π]` first.
    /// Max error ≈ 0.001 — plenty for LFOs / ring modulation.
    pub fn sin(mut x: f32) -> f32 {
        while x > PI {
            x -= TAU;
        }
        while x < -PI {
            x += TAU;
        }
        const B: f32 = 4.0 / PI;
        const C: f32 = -4.0 / (PI * PI);
        let y = B * x + C * x * x.abs();
        0.225 * (y * y.abs() - y) + y
    }

    /// Rational (Padé) `tanh` approximation, saturating outside ±3.
    pub fn tanh(x: f32) -> f32 {
        if x < -3.0 {
            return -1.0;
        }
        if x > 3.0 {
            return 1.0;
        }
        let x2 = x * x;
        x * (27.0 + x2) / (27.0 + 9.0 * x2)
    }
}

/// A mono DSP processor. One instance per node, driven a quantum at a time on
/// the audio thread.
pub trait Processor {
    /// Declared parameters (≤ [`MAX_PARAMS`]).
    const PARAMS: &'static [ParamDesc];
    /// Construct (called once, on the audio thread). `sample_rate` is the
    /// context's rate in Hz (e.g. for computing oscillator/filter coefficients).
    fn new(sample_rate: f32) -> Self;
    /// Process one render quantum. `input[ch]` / `output[ch]` are per-channel
    /// sample slices ([`CHANNELS`] of them, equal length). Must not allocate.
    fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], params: &Params);
}

/// Generate the awsm-audio worklet ABI exports for a [`Processor`] type. Invoke
/// exactly once at crate root.
///
/// Two forms:
/// - `awsm_worklet!(MyProc);` — for a normal (std) crate; std supplies the
///   panic handler the final `cdylib` needs.
/// - `awsm_worklet!(MyProc, no_std);` — for a `#![no_std]` crate: *also* emits a
///   minimal `#[panic_handler]`. A `#![no_std]` cdylib **must** define one, and
///   without it the build fails at link with the cryptic
///   `` error: `#[panic_handler]` function required, but not found ``. Use this
///   form (don't hand-write the handler) so the no_std path is turnkey.
///
/// ```ignore
/// #![no_std]
/// use awsm_audio_worklet::*;
/// // ... impl Processor for MyProc ...
/// awsm_worklet!(MyProc, no_std); // emits the ABI exports + a panic handler
/// ```
#[macro_export]
macro_rules! awsm_worklet {
    // `#![no_std]` crates: the regular ABI exports plus the required panic
    // handler (a bare spin — DSP code shouldn't panic on the audio thread, and
    // there's nowhere to unwind to). Don't combine with another crate that also
    // defines `#[panic_handler]` (e.g. `panic-halt`), or you'll get a duplicate
    // lang-item error.
    ($ty:ty, no_std) => {
        $crate::awsm_worklet!($ty);

        #[panic_handler]
        fn __awsm_worklet_panic(_: &::core::panic::PanicInfo) -> ! {
            loop {}
        }
    };
    ($ty:ty) => {
        static mut __AWSM_INPUT: [f32; $crate::CHANNELS * $crate::MAX_FRAMES] =
            [0.0; $crate::CHANNELS * $crate::MAX_FRAMES];
        static mut __AWSM_OUTPUT: [f32; $crate::CHANNELS * $crate::MAX_FRAMES] =
            [0.0; $crate::CHANNELS * $crate::MAX_FRAMES];
        static mut __AWSM_PARAMS: [f32; $crate::MAX_PARAMS] = [0.0; $crate::MAX_PARAMS];
        static mut __AWSM_PROC: ::core::option::Option<$ty> = ::core::option::Option::None;
        static mut __AWSM_SR: f32 = 48000.0;

        #[no_mangle]
        pub extern "C" fn init(sample_rate: f32, _max_frames: u32) {
            unsafe {
                *::core::ptr::addr_of_mut!(__AWSM_SR) = sample_rate;
                *::core::ptr::addr_of_mut!(__AWSM_PROC) =
                    ::core::option::Option::Some(<$ty as $crate::Processor>::new(sample_rate));
            }
        }

        #[no_mangle]
        pub extern "C" fn input_ptr() -> u32 {
            ::core::ptr::addr_of!(__AWSM_INPUT) as u32
        }
        #[no_mangle]
        pub extern "C" fn output_ptr() -> u32 {
            ::core::ptr::addr_of!(__AWSM_OUTPUT) as u32
        }
        #[no_mangle]
        pub extern "C" fn params_ptr() -> u32 {
            ::core::ptr::addr_of!(__AWSM_PARAMS) as u32
        }

        #[no_mangle]
        pub extern "C" fn channels() -> u32 {
            $crate::CHANNELS as u32
        }
        #[no_mangle]
        pub extern "C" fn param_count() -> u32 {
            <$ty as $crate::Processor>::PARAMS.len() as u32
        }
        #[no_mangle]
        pub extern "C" fn param_name_ptr(i: u32) -> u32 {
            <$ty as $crate::Processor>::PARAMS[i as usize].name.as_ptr() as u32
        }
        #[no_mangle]
        pub extern "C" fn param_name_len(i: u32) -> u32 {
            <$ty as $crate::Processor>::PARAMS[i as usize].name.len() as u32
        }
        #[no_mangle]
        pub extern "C" fn param_min(i: u32) -> f32 {
            <$ty as $crate::Processor>::PARAMS[i as usize].min
        }
        #[no_mangle]
        pub extern "C" fn param_max(i: u32) -> f32 {
            <$ty as $crate::Processor>::PARAMS[i as usize].max
        }
        #[no_mangle]
        pub extern "C" fn param_default(i: u32) -> f32 {
            <$ty as $crate::Processor>::PARAMS[i as usize].default
        }

        #[no_mangle]
        pub extern "C" fn process(frames: u32) {
            let n = (frames as usize).min($crate::MAX_FRAMES);
            let pc = <$ty as $crate::Processor>::PARAMS
                .len()
                .min($crate::MAX_PARAMS);
            unsafe {
                let proc_ptr = ::core::ptr::addr_of_mut!(__AWSM_PROC);
                if (*proc_ptr).is_none() {
                    let sr = *::core::ptr::addr_of!(__AWSM_SR);
                    *proc_ptr = ::core::option::Option::Some(<$ty as $crate::Processor>::new(sr));
                }
                // Per-channel (planar) slices over the scratch regions; each
                // channel `c` occupies `[c*MAX_FRAMES .. c*MAX_FRAMES + n]`.
                let in_base = ::core::ptr::addr_of!(__AWSM_INPUT) as *const f32;
                let out_base = ::core::ptr::addr_of_mut!(__AWSM_OUTPUT) as *mut f32;
                let input: [&[f32]; $crate::CHANNELS] = ::core::array::from_fn(|c| unsafe {
                    ::core::slice::from_raw_parts(in_base.add(c * $crate::MAX_FRAMES), n)
                });
                let mut output: [&mut [f32]; $crate::CHANNELS] =
                    ::core::array::from_fn(|c| unsafe {
                        ::core::slice::from_raw_parts_mut(out_base.add(c * $crate::MAX_FRAMES), n)
                    });
                let params = $crate::Params {
                    values: ::core::slice::from_raw_parts(
                        ::core::ptr::addr_of!(__AWSM_PARAMS) as *const f32,
                        pc,
                    ),
                };
                if let ::core::option::Option::Some(p) = (*proc_ptr).as_mut() {
                    p.process(&input, &mut output, &params);
                }
            }
        }
    };
}

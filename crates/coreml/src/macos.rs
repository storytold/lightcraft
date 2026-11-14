//! The calls into Core ML (through the objc2 bindings). This is the only file of the crate with `unsafe`: the bindings
//! mark every Objective-C message `unsafe` because they cannot check receivers and argument types. Each block says
//! why it is sound. Nothing here panics: framework errors become [`Error`]s, and every object lives inside an
//! autorelease pool that ends with the call, so repeated predictions do not pile up memory.

use crate::layout::{self, Dtype};
use crate::{ComputeUnits, Error, Tensor};
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel, MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSInteger, NSNumber, NSString, NSURL};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

/// Core ML's own description of an error.
fn describe(e: &NSError) -> String {
    e.localizedDescription().to_string()
}

/// A file URL for `path`; an error when the path is not valid UTF-8.
fn url_of(path: &Path) -> Result<Retained<NSURL>, Error> {
    let s = path.to_str().ok_or_else(|| Error::Input(format!("{} is not valid UTF-8", path.display())))?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(s)))
}

/// The numbers of an `NSArray<NSNumber>` as sizes (a negative one is an error).
fn sizes(array: &NSArray<NSNumber>) -> Result<Vec<usize>, Error> {
    array.to_vec().iter().map(|n| usize::try_from(n.as_isize()).map_err(|_| Error::Output("a negative size or stride".into()))).collect()
}

/// Where the compiled model of `source` is kept in `dest_dir`: `<file name without .mlmodel>.mlmodelc`. Only the
/// `.mlmodel` suffix goes, so `model.v2.mlmodel` and `model.v3.mlmodel` keep apart.
fn target_of(source: &Path, dest_dir: &Path) -> Result<PathBuf, Error> {
    let name = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| Error::Input("the model has no usable file name".into()))?;
    let stem = name.strip_suffix(".mlmodel").unwrap_or(name);
    if stem.is_empty() {
        return Err(Error::Input("the model has no file name".into()));
    }
    Ok(dest_dir.join(format!("{stem}.mlmodelc")))
}

/// Compiles the model at `source` into `dest_dir` (see the crate's `compile`), removing Core ML's temporary folder on every path.
pub fn compile(source: &Path, dest_dir: &Path) -> Result<PathBuf, Error> {
    autoreleasepool(|_| {
        let target = target_of(source, dest_dir)?;
        let url = url_of(source)?;
        // SAFETY: `url` is a valid file URL object; the call only reads the model file and returns a URL of the
        // compiled folder in a temporary place, or an error.
        #[allow(deprecated)]
        let compiled = unsafe { MLModel::compileModelAtURL_error(&url) }.map_err(|e| Error::Compile(describe(&e)))?;
        let made = compiled.path().map(|p| PathBuf::from(p.to_string())).ok_or_else(|| Error::Compile("Core ML gave no path".into()))?;
        let kept = (|| {
            std::fs::create_dir_all(dest_dir).map_err(|e| Error::Compile(format!("{}: {e}", dest_dir.display())))?;
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| Error::Compile(format!("{}: {e}", target.display())))?;
            }
            // the temporary folder may be on another volume: copy then
            if std::fs::rename(&made, &target).is_err()
                && let Err(e) = copy_dir(&made, &target, 0)
            {
                let _ = std::fs::remove_dir_all(&target);
                return Err(Error::Compile(format!("{}: {e}", target.display())));
            }
            Ok(())
        })();
        // whatever happened, Core ML's temporary folder is not left behind (it is gone after a rename)
        let _ = std::fs::remove_dir_all(&made);
        kept.map(|()| target)
    })
}

/// Copy a folder (a compiled model is a few levels deep, never a link loop: the depth is capped anyway).
fn copy_dir(from: &Path, to: &Path, depth: usize) -> std::io::Result<()> {
    if depth > 8 {
        return Err(std::io::Error::other("folders nested too deeply"));
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_dir(&src, &dst, depth + 1)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The model object, movable between threads because every use takes the lock around it.
struct Handle(Retained<MLModel>);

// SAFETY: an Objective-C object's reference count is atomic, and `Loaded` lets only one thread at a time use the model
// (the `Mutex` around the handle), so the object is never used from two threads at once.
unsafe impl Send for Handle {}

pub struct Loaded {
    model: Mutex<Handle>,
    input: Retained<NSString>,
    output: Retained<NSString>,
}

// SAFETY: the two `NSString`s are immutable objects that are only read; the model sits behind a `Mutex`.
unsafe impl Send for Loaded {}
// SAFETY: as above: shared use only reads the immutable names and takes the lock for the model.
unsafe impl Sync for Loaded {}

/// Core ML's setting for where a model may run.
fn units(u: ComputeUnits) -> MLComputeUnits {
    match u {
        ComputeUnits::CpuOnly => MLComputeUnits::CPUOnly,
        ComputeUnits::CpuAndGpu => MLComputeUnits::CPUAndGPU,
        ComputeUnits::CpuAndNeuralEngine => MLComputeUnits::CPUAndNeuralEngine,
        ComputeUnits::All => MLComputeUnits::All,
    }
}

impl Loaded {
    /// Loads a compiled model for `u`; the names of its input and output are used by [`Loaded::predict`], which says so
    /// when the model has no such feature.
    pub fn load(compiled: &Path, u: ComputeUnits, input: &str, output: &str) -> Result<Loaded, Error> {
        autoreleasepool(|_| {
            if !compiled.exists() {
                return Err(Error::Load(format!("{} does not exist", compiled.display())));
            }
            let url = url_of(compiled)?;
            // SAFETY: a fresh configuration object, and a setter taking a plain enum value.
            let config = unsafe {
                let c = MLModelConfiguration::new();
                c.setComputeUnits(units(u));
                c
            };
            // SAFETY: `url` and `config` are valid objects; the call reads the compiled model and returns a model or
            // an error.
            let model = unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url, &config) }.map_err(|e| Error::Load(describe(&e)))?;
            Ok(Loaded { model: Mutex::new(Handle(model)), input: NSString::from_str(input), output: NSString::from_str(output) })
        })
    }

    /// Runs the model on `data` (shaped `shape`) and returns the output array.
    pub fn predict(&self, shape: &[usize], data: &[f32]) -> Result<Tensor, Error> {
        autoreleasepool(|_| {
            let features = self.features(shape, data)?;
            let answer = {
                let model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
                // SAFETY: `features` is a live feature provider holding the one input the model was loaded for; the
                // call returns a provider or an error, and the lock serializes uses of the model.
                unsafe { model.0.predictionFromFeatures_error(ProtocolObject::from_ref(&*features)) }.map_err(|e| Error::Output(describe(&e)))?
            };
            // SAFETY: `answer` is a live feature provider and the name an `NSString`.
            let value = unsafe { answer.featureValueForName(&self.output) }.ok_or_else(|| Error::Output("the model has no such output".into()))?;
            // SAFETY: `value` is a live feature value; the accessor only reads it.
            let array = unsafe { value.multiArrayValue() }.ok_or_else(|| Error::Output("the output is not an array".into()))?;
            read_array(&array)
        })
    }

    /// The provider holding `data` as the model's input.
    fn features(&self, shape: &[usize], data: &[f32]) -> Result<Retained<MLDictionaryFeatureProvider>, Error> {
        let dims: Vec<Retained<NSNumber>> = shape.iter().map(|&d| NSNumber::new_isize(d as isize)).collect();
        let dims = NSArray::from_retained_slice(&dims);
        // SAFETY: a fresh allocation initialised by the documented initialiser, with a valid shape array.
        let array = unsafe { MLMultiArray::initWithShape_dataType_error(MLMultiArray::alloc(), &dims, MLMultiArrayDataType::Float32) }
            .map_err(|e| Error::Input(describe(&e)))?;
        let filled = Cell::new(false);
        let fill = RcBlock::new(|bytes: NonNull<c_void>, size: NSInteger, strides: NonNull<NSArray<NSNumber>>| {
            let want = std::mem::size_of_val(data);
            // SAFETY: Core ML passes a valid strides array for the duration of this call.
            let contiguous = sizes(unsafe { strides.as_ref() }).is_ok_and(|s| s == layout::contiguous_strides(shape));
            if contiguous && usize::try_from(size).is_ok_and(|size| size >= want) {
                // SAFETY: `bytes` points at `size` writable bytes (at least `want`) for the duration of this call, and
                // `data` holds `want` readable bytes; the two cannot overlap (the array was just allocated).
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), bytes.as_ptr().cast::<u8>(), want) };
                filled.set(true);
            }
        });
        // SAFETY: the block only runs during the call, and what it captures outlives it.
        unsafe { array.getMutableBytesWithHandler(&fill) };
        if !filled.get() {
            return Err(Error::Input("Core ML's input array is not laid out as expected".into()));
        }
        // SAFETY: `array` is a live multi-array.
        let value = unsafe { MLFeatureValue::featureValueWithMultiArray(&array) };
        // SAFETY: an `MLFeatureValue` is an Objective-C object, so it can be held as `AnyObject`.
        let value: Retained<AnyObject> = unsafe { Retained::cast_unchecked(value) };
        let dict = NSDictionary::from_retained_objects(&[&*self.input], &[value]);
        // SAFETY: a fresh allocation initialised by the documented initialiser; the dictionary maps a name to a
        // feature value, as the initialiser requires.
        unsafe { MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dict) }
            .map_err(|e| Error::Input(describe(&e)))
    }
}

/// What a multi-array holds, as a tensor.
fn read_array(array: &MLMultiArray) -> Result<Tensor, Error> {
    // SAFETY: `array` is a live multi-array; the accessors only read it.
    let (dtype, shape, strides) = unsafe { (array.dataType(), sizes(&array.shape())?, sizes(&array.strides())?) };
    let dtype = if dtype == MLMultiArrayDataType::Float32 {
        Dtype::F32
    } else if dtype == MLMultiArrayDataType::Float16 {
        Dtype::F16
    } else if dtype == MLMultiArrayDataType::Float64 {
        Dtype::F64
    } else {
        return Err(Error::Output("the output is not made of floating-point numbers".into()));
    };
    let copy = RefCell::new(Vec::new());
    let take = RcBlock::new(|bytes: NonNull<c_void>, size: NSInteger| {
        if let Ok(size) = usize::try_from(size).map(|s| s.min(layout::MAX_ELEMENTS.saturating_mul(8))) {
            // SAFETY: Core ML guarantees `bytes` points at `size` readable bytes for the duration of this call.
            *copy.borrow_mut() = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), size) }.to_vec();
        }
    });
    // SAFETY: the block only runs during the call, and what it captures outlives it.
    unsafe { array.getBytesWithHandler(&take) };
    let data = layout::gather(&copy.borrow(), dtype, &shape, &strides)?;
    Ok(Tensor { shape, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: compiled models of dotted names keep apart.
    #[test]
    fn compiled_models_of_dotted_names_keep_apart() {
        let dir = Path::new("/models");
        let a = target_of(Path::new("/x/model.v2.mlmodel"), dir).unwrap();
        let b = target_of(Path::new("/x/model.v3.mlmodel"), dir).unwrap();
        assert_eq!(a, Path::new("/models/model.v2.mlmodelc"));
        assert_ne!(a, b);
        assert_eq!(target_of(Path::new("net.mlmodel"), dir).unwrap(), Path::new("/models/net.mlmodelc"));
        // a file that is not named .mlmodel keeps its whole name
        assert_eq!(target_of(Path::new("net.bin"), dir).unwrap(), Path::new("/models/net.bin.mlmodelc"));
        assert!(target_of(Path::new(""), dir).is_err());
        assert!(target_of(Path::new(".mlmodel"), dir).is_err());
    }
}

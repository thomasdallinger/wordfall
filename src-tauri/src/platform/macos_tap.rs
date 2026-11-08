//! System audio on macOS: a Core Audio process tap (macOS 14.2+) of everything except Wordfall,
//! read through a private aggregate device. macOS asks for "System Audio Recording" permission
//! the first time; without it the tap delivers silence.
//!
//! The tap functions only exist on 14.2 and later, so they are looked up at run time; a static
//! reference would keep Wordfall from launching on macOS 13.

use std::{
    ffi::{c_void, CStr},
    ptr::NonNull,
};

use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    string::{CFString, CFStringRef},
};
use objc2::{rc::Retained, AllocAnyThread};
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultSystemOutputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{kAudioFormatFlagIsFloat, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_foundation::{NSArray, NSNumber, NSProcessInfo, NSString, NSUUID};

/// Receives mono samples at `SystemTap::sample_rate`, on Core Audio's real-time thread.
pub type Sink = Box<dyn FnMut(&[f32]) + Send>;

pub struct SystemTap {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    rate: u32,
    /// Owned by the IO proc while it runs; freed after it has been stopped.
    context: *mut Context,
}

// SAFETY: the ids are plain integers; `context` is only touched by the IO proc and by `drop`
// after the IO proc has been stopped.
unsafe impl Send for SystemTap {}

struct Context {
    sink: Sink,
    scratch: Vec<f32>,
}

type CreateTap = unsafe extern "C" fn(*const CATapDescription, *mut AudioObjectID) -> i32;
type DestroyTap = unsafe extern "C" fn(AudioObjectID) -> i32;

fn lookup<T>(name: &CStr) -> Option<T> {
    // SAFETY: dlsym with RTLD_DEFAULT just searches the loaded images; the caller names the type.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
}

/// Whether this macOS can tap system audio at all (14.2 or later).
pub fn system_audio_supported() -> bool {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    (v.majorVersion, v.minorVersion) >= (14, 2) && lookup::<CreateTap>(c"AudioHardwareCreateProcessTap").is_some()
}

fn check(status: i32, what: &str) -> anyhow::Result<()> {
    anyhow::ensure!(status == 0, "{what} failed (OSStatus {status})");
    Ok(())
}

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property, optionally with qualifier data.
unsafe fn get_property<T: Copy>(object: AudioObjectID, selector: u32, qualifier: Option<&[u8]>) -> anyhow::Result<T> {
    let addr = address(selector);
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>() as u32;
    let (qsize, qdata) = qualifier.map(|q| (q.len() as u32, q.as_ptr().cast())).unwrap_or((0, std::ptr::null()));
    check(
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            qsize,
            qdata,
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast()),
        ),
        "reading an audio property",
    )?;
    Ok(value.assume_init())
}

fn key(k: &CStr) -> CFString {
    CFString::new(k.to_str().unwrap_or_default())
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let Some(ctx) = (client as *mut Context).as_mut() else { return 0 };
    let list = input.as_ptr();
    let count = (*list).mNumberBuffers as usize;
    let buffers = std::slice::from_raw_parts((*list).mBuffers.as_ptr(), count);
    // Interleaved (one buffer, n channels) or planar (n buffers, one channel each): mix to mono.
    let total_channels: usize = buffers.iter().map(|b| b.mNumberChannels.max(1) as usize).sum();
    let Some(first) = buffers.first() else { return 0 };
    let frames = first.mDataByteSize as usize / 4 / first.mNumberChannels.max(1) as usize;
    ctx.scratch.clear();
    ctx.scratch.resize(frames, 0.0);
    for b in buffers {
        if b.mData.is_null() {
            continue;
        }
        let ch = b.mNumberChannels.max(1) as usize;
        let data = std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4);
        for (i, out) in ctx.scratch.iter_mut().enumerate() {
            for c in 0..ch {
                *out += data.get(i * ch + c).copied().unwrap_or(0.0);
            }
        }
    }
    let scale = 1.0 / total_channels.max(1) as f32;
    for s in ctx.scratch.iter_mut() {
        *s *= scale;
    }
    let Context { sink, scratch } = ctx;
    sink(scratch);
    0
}

impl SystemTap {
    pub fn start(sink: Sink) -> anyhow::Result<Self> {
        anyhow::ensure!(system_audio_supported(), "recording other apps' audio needs macOS 14.2 or later");
        let create: CreateTap = lookup(c"AudioHardwareCreateProcessTap").expect("checked above");
        let destroy: DestroyTap =
            lookup(c"AudioHardwareDestroyProcessTap").ok_or_else(|| anyhow::anyhow!("process taps are unavailable"))?;
        unsafe {
            // Leave Wordfall's own sounds (the dictation cues) out of the recording.
            let pid = std::process::id() as i32;
            let own: Option<AudioObjectID> = get_property(
                kAudioObjectSystemObject as AudioObjectID,
                kAudioHardwarePropertyTranslatePIDToProcessObject,
                Some(&pid.to_ne_bytes()),
            )
            .ok()
            .filter(|id: &AudioObjectID| *id != 0);
            let exclude: Retained<NSArray<NSNumber>> = match own {
                Some(id) => NSArray::from_retained_slice(&[NSNumber::new_u32(id)]),
                None => NSArray::new(),
            };
            let desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &exclude);
            let tap_uuid = NSUUID::new();
            desc.setUUID(&tap_uuid);
            desc.setName(&NSString::from_str("Wordfall meeting"));
            desc.setPrivate(true);

            let mut tap: AudioObjectID = 0;
            check(create(Retained::as_ptr(&desc), &mut tap), "creating the system audio tap")?;
            let cleanup_tap = |tap| {
                destroy(tap);
            };

            let built = (|| -> anyhow::Result<(AudioObjectID, AudioDeviceIOProcID, u32, *mut Context)> {
                let format: AudioStreamBasicDescription = get_property(tap, kAudioTapPropertyFormat, None)?;
                anyhow::ensure!(
                    format.mFormatFlags & kAudioFormatFlagIsFloat != 0 && format.mBitsPerChannel == 32,
                    "unexpected system audio format"
                );
                // The aggregate device runs on the current output device's clock.
                let output: AudioObjectID =
                    get_property(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyDefaultSystemOutputDevice, None)?;
                let output_uid_ref: CFStringRef = get_property(output, kAudioDevicePropertyDeviceUID, None)?;
                anyhow::ensure!(!output_uid_ref.is_null(), "the output device has no id");
                let output_uid = CFString::wrap_under_create_rule(output_uid_ref);
                let tap_uid = CFString::new(&tap_uuid.UUIDString().to_string());

                let sub_device = CFDictionary::from_CFType_pairs(&[(key(kAudioSubDeviceUIDKey).as_CFType(), output_uid.as_CFType())]);
                let sub_tap = CFDictionary::from_CFType_pairs(&[
                    (key(kAudioSubTapUIDKey).as_CFType(), tap_uid.as_CFType()),
                    (key(kAudioSubTapDriftCompensationKey).as_CFType(), CFBoolean::true_value().as_CFType()),
                ]);
                let aggregate_desc: CFDictionary<CFType, CFType> = CFDictionary::from_CFType_pairs(&[
                    (key(kAudioAggregateDeviceNameKey).as_CFType(), CFString::new("Wordfall meeting").as_CFType()),
                    (
                        key(kAudioAggregateDeviceUIDKey).as_CFType(),
                        CFString::new(&NSUUID::new().UUIDString().to_string()).as_CFType(),
                    ),
                    (key(kAudioAggregateDeviceMainSubDeviceKey).as_CFType(), output_uid.as_CFType()),
                    (key(kAudioAggregateDeviceIsPrivateKey).as_CFType(), CFBoolean::true_value().as_CFType()),
                    (key(kAudioAggregateDeviceIsStackedKey).as_CFType(), CFBoolean::false_value().as_CFType()),
                    (key(kAudioAggregateDeviceTapAutoStartKey).as_CFType(), CFBoolean::true_value().as_CFType()),
                    (
                        key(kAudioAggregateDeviceSubDeviceListKey).as_CFType(),
                        CFArray::from_CFTypes(&[sub_device]).as_CFType(),
                    ),
                    (key(kAudioAggregateDeviceTapListKey).as_CFType(), CFArray::from_CFTypes(&[sub_tap]).as_CFType()),
                ]);
                let mut aggregate: AudioObjectID = 0;
                let dict = &*(aggregate_desc.as_concrete_TypeRef() as *const objc2_core_foundation::CFDictionary);
                check(AudioHardwareCreateAggregateDevice(dict, NonNull::from(&mut aggregate)), "creating the capture device")?;

                let context = Box::into_raw(Box::new(Context { sink, scratch: Vec::with_capacity(4096) }));
                let mut proc_id: AudioDeviceIOProcID = None;
                let status = AudioDeviceCreateIOProcID(aggregate, Some(io_proc), context.cast(), NonNull::from(&mut proc_id));
                if status != 0 {
                    drop(Box::from_raw(context));
                    AudioHardwareDestroyAggregateDevice(aggregate);
                    check(status, "starting system audio capture")?;
                }
                let status = AudioDeviceStart(aggregate, proc_id);
                if status != 0 {
                    AudioDeviceDestroyIOProcID(aggregate, proc_id);
                    drop(Box::from_raw(context));
                    AudioHardwareDestroyAggregateDevice(aggregate);
                    check(status, "starting system audio capture")?;
                }
                Ok((aggregate, proc_id, format.mSampleRate as u32, context))
            })();
            match built {
                Ok((aggregate, proc_id, rate, context)) => {
                    log::info!("system audio tap running at {rate} Hz");
                    Ok(Self { tap, aggregate, proc_id, rate, context })
                }
                Err(e) => {
                    cleanup_tap(tap);
                    Err(e)
                }
            }
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
}

impl Drop for SystemTap {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.aggregate, self.proc_id);
            AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.aggregate);
            if let Some(destroy) = lookup::<DestroyTap>(c"AudioHardwareDestroyProcessTap") {
                destroy(self.tap);
            }
            drop(Box::from_raw(self.context));
        }
    }
}

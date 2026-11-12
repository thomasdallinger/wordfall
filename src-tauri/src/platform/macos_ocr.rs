//! Text in images with Apple's Vision framework (`VNRecognizeTextRequest`, accurate, with language
//! correction), and images resized and turned upright with ImageIO. Both are called through the
//! Objective-C runtime and C functions, so no extra crate is needed. Vision reads an image in
//! about 0.3–0.5 s on Apple silicon; ImageIO decodes PNG, JPEG, HEIC, TIFF and more.

use std::{ffi::c_void, path::Path};

use core_foundation::{
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
    url::{CFURLRef, CFURL},
};
use objc2::{
    class, msg_send,
    rc::{autoreleasepool, Allocated, Retained},
    runtime::AnyObject,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSRect, NSString, NSURL};

use super::OcrLine;

#[link(name = "Vision", kind = "framework")]
extern "C" {}

type CFDictionaryRef = core_foundation::dictionary::CFDictionaryRef;

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageSourceCreateWithURL(url: CFURLRef, options: CFDictionaryRef) -> *const c_void;
    fn CGImageSourceCreateThumbnailAtIndex(source: *const c_void, index: usize, options: CFDictionaryRef) -> *const c_void;
    fn CGImageDestinationCreateWithURL(url: CFURLRef, kind: CFStringRef, count: usize, options: CFDictionaryRef) -> *const c_void;
    fn CGImageDestinationAddImage(dest: *const c_void, image: *const c_void, properties: CFDictionaryRef);
    fn CGImageDestinationFinalize(dest: *const c_void) -> bool;
    static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;
    static kCGImageDestinationLossyCompressionQuality: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// Releases a Core Foundation object it owns when dropped.
struct Owned(*const c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Vision's name of a language (ISO 639-1), for the ones it reads.
fn vision_language(code: &str) -> Option<&'static str> {
    Some(match code {
        "en" => "en-US",
        "de" => "de-DE",
        "fr" => "fr-FR",
        "it" => "it-IT",
        "es" => "es-ES",
        "pt" => "pt-BR",
        "zh" => "zh-Hans",
        "ja" => "ja-JP",
        "ko" => "ko-KR",
        "ru" => "ru-RU",
        "uk" => "uk-UA",
        _ => return None,
    })
}

/// The text in the image at `path`: a box per line or table cell, in no particular order.
/// `languages` (ISO 639-1, the user's first) guide recognition; English is always added.
/// `words` are names and terms to prefer (the user's dictionary).
pub fn ocr(path: &Path, languages: &[String], words: &[String]) -> anyhow::Result<Vec<OcrLine>> {
    anyhow::ensure!(path.exists(), "{} is gone", path.display());
    let mut wanted: Vec<&str> = languages.iter().filter_map(|l| vision_language(l)).collect();
    if !wanted.contains(&"en-US") {
        wanted.push("en-US");
    }
    autoreleasepool(|_| unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let options = NSDictionary::<NSString, AnyObject>::new();
        let alloc: Allocated<AnyObject> = msg_send![class!(VNImageRequestHandler), alloc];
        let handler: Retained<AnyObject> = msg_send![alloc, initWithURL: &*url, options: &*options];
        let alloc: Allocated<AnyObject> = msg_send![class!(VNRecognizeTextRequest), alloc];
        let request: Retained<AnyObject> = msg_send![alloc, init];
        // VNRequestTextRecognitionLevelAccurate
        let _: () = msg_send![&*request, setRecognitionLevel: 0isize];
        let _: () = msg_send![&*request, setUsesLanguageCorrection: true];
        // Only languages this macOS reads (an unknown one fails the whole request).
        let supported: Result<Retained<NSArray<NSString>>, Retained<NSError>> =
            msg_send![&*request, supportedRecognitionLanguagesAndReturnError: _];
        let supported: Vec<String> = supported.map(|a| a.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        let languages: Vec<Retained<NSString>> =
            wanted.iter().filter(|l| supported.is_empty() || supported.iter().any(|s| s == *l)).map(|l| NSString::from_str(l)).collect();
        if !languages.is_empty() {
            let _: () = msg_send![&*request, setRecognitionLanguages: &*NSArray::from_retained_slice(&languages)];
        }
        let words: Vec<Retained<NSString>> = words.iter().filter(|w| !w.trim().is_empty()).take(200).map(|w| NSString::from_str(w.trim())).collect();
        if !words.is_empty() {
            let _: () = msg_send![&*request, setCustomWords: &*NSArray::from_retained_slice(&words)];
        }
        let requests = NSArray::<AnyObject>::from_slice(&[&*request]);
        let done: Result<(), Retained<NSError>> = msg_send![&*handler, performRequests: &*requests, error: _];
        done.map_err(|e| anyhow::anyhow!("text recognition failed: {}", e.localizedDescription()))?;
        let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let mut out = Vec::new();
        for observation in results.iter().flat_map(|r| r.iter()) {
            let candidates: Retained<NSArray<AnyObject>> = msg_send![&*observation, topCandidates: 1usize];
            let Some(best) = candidates.firstObject() else { continue };
            let text: Retained<NSString> = msg_send![&*best, string];
            // Normalised, origin at the bottom left.
            let b: NSRect = msg_send![&*observation, boundingBox];
            out.push(OcrLine {
                text: text.to_string(),
                x: b.origin.x as f32,
                y: (1.0 - b.origin.y - b.size.height) as f32,
                w: b.size.width as f32,
                h: b.size.height as f32,
            });
        }
        Ok(out)
    })
}

/// Writes the image at `from` to `to` as a JPEG of at most `max_px` on its longer side, turned
/// the way it was taken (phone photos carry their rotation as metadata). Returns its size.
pub fn normalize_image(from: &Path, to: &Path, max_px: u32) -> anyhow::Result<(u32, u32)> {
    let src_url = CFURL::from_path(from, false).ok_or_else(|| anyhow::anyhow!("bad path {}", from.display()))?;
    let dst_url = CFURL::from_path(to, false).ok_or_else(|| anyhow::anyhow!("bad path {}", to.display()))?;
    unsafe {
        let source = Owned(CGImageSourceCreateWithURL(src_url.as_concrete_TypeRef(), std::ptr::null()));
        anyhow::ensure!(!source.0.is_null(), "{} isn't an image this Mac can read", from.display());
        let options: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[
            (CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailFromImageAlways), CFBoolean::true_value().as_CFType()),
            (CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailWithTransform), CFBoolean::true_value().as_CFType()),
            (CFString::wrap_under_get_rule(kCGImageSourceThumbnailMaxPixelSize), CFNumber::from(max_px as i64).as_CFType()),
        ]);
        let image = Owned(CGImageSourceCreateThumbnailAtIndex(source.0, 0, options.as_concrete_TypeRef()));
        anyhow::ensure!(!image.0.is_null(), "{} isn't an image this Mac can read", from.display());
        let size = (CGImageGetWidth(image.0) as u32, CGImageGetHeight(image.0) as u32);
        let jpeg = CFString::new("public.jpeg");
        let dest = Owned(CGImageDestinationCreateWithURL(dst_url.as_concrete_TypeRef(), jpeg.as_concrete_TypeRef(), 1, std::ptr::null()));
        anyhow::ensure!(!dest.0.is_null(), "can't write {}", to.display());
        let quality: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[(
            CFString::wrap_under_get_rule(kCGImageDestinationLossyCompressionQuality),
            CFNumber::from(0.8f64).as_CFType(),
        )]);
        CGImageDestinationAddImage(dest.0, image.0, quality.as_concrete_TypeRef());
        anyhow::ensure!(CGImageDestinationFinalize(dest.0), "writing {} failed", to.display());
        Ok(size)
    }
}

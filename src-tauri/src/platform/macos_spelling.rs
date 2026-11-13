//! Whether a word is an ordinary word of a language, by the system spell checker (`NSSpellChecker`,
//! the one every Mac text field uses). The meeting vocabulary uses it to tell a product name on a
//! shared screen ("Brontix", "TreVolin") from the ordinary words around it ("Eigenschaften",
//! "Overview"), without shipping a dictionary. Called through the Objective-C runtime, like the
//! OCR, so no extra crate feature is needed. Answers are kept: the live views ask again and again.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use objc2::{
    class, msg_send,
    rc::{autoreleasepool, Retained},
    runtime::AnyObject,
};
use objc2_foundation::{NSRange, NSString};

/// `NSNotFound`: where `checkSpellingOfString:` says the first misspelled word is when there is none.
const NOT_FOUND: usize = isize::MAX as usize;

fn known() -> &'static Mutex<HashMap<(String, String), bool>> {
    static KNOWN: OnceLock<Mutex<HashMap<(String, String), bool>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// Whether `word` is spelled right in one of `languages` ("de", "en"; English is always asked).
/// A language the spell checker doesn't have says no. Words the user taught the spell checker
/// count too: they are words the user writes, not new names.
pub fn is_word(word: &str, languages: &[String]) -> bool {
    let mut asked: Vec<&str> = languages.iter().map(String::as_str).collect();
    if !asked.contains(&"en") {
        asked.push("en");
    }
    asked.into_iter().any(|lang| spelled_right(word, lang))
}

fn spelled_right(word: &str, language: &str) -> bool {
    let key = (word.to_string(), language.to_string());
    if let Some(&k) = known().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return k;
    }
    let right = autoreleasepool(|_| unsafe {
        let checker: Option<Retained<AnyObject>> = msg_send![class!(NSSpellChecker), sharedSpellChecker];
        let Some(checker) = checker else { return false };
        let text = NSString::from_str(word);
        let lang = NSString::from_str(language);
        let found: NSRange = msg_send![
            &*checker,
            checkSpellingOfString: &*text,
            startingAt: 0isize,
            language: &*lang,
            wrap: false,
            inSpellDocumentWithTag: 0isize,
            wordCount: std::ptr::null_mut::<isize>()
        ];
        found.location == NOT_FOUND
    });
    known().lock().unwrap_or_else(|e| e.into_inner()).insert(key, right);
    right
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system's dictionaries: an ordinary German and English word, and a made-up name.
    #[test]
    fn ordinary_words_and_made_up_names() {
        let langs = ["de".to_string()];
        assert!(is_word("Eigenschaften", &langs));
        assert!(is_word("Overview", &langs));
        assert!(!is_word("Brontixwerk", &langs));
    }
}

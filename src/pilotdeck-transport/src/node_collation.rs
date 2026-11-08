//! Shared pure Node/ICU locale profile, moved from gateway helpers.
//! Uses the workspace-pinned ICU78 data and its existing fr-CA repair.
use icu_collator::{Collator,CollatorBorrowed,CollatorPreferences};
use icu_locale_core::Locale;

pub fn node_locale(env: impl FnMut(&str) -> Option<String>) -> Locale {
    let value = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(env)
        .unwrap_or_else(|| "en-US".into());
    // Empty values deliberately win precedence: Node resolves them to und.
    let (base, modifier) = value.split_once('@').unwrap_or((&value, ""));
    let language = base.split('.').next().unwrap_or("").replace('_', "-");
    if language == "C" || language == "POSIX" {
        return "en-US".parse().expect("locale literal");
    }
    // ICU's POSIX parser keeps the valid prefix of malformed locale names;
    // a single-letter component is not a POSIX language/script/region/variant.
    let mut prefix = language
        .split('-')
        .take_while(|part| part.len() != 1)
        .collect::<Vec<_>>();
    let mut locale = loop {
        if prefix.is_empty() {
            break Locale::UNKNOWN;
        }
        if let Ok(locale) = prefix.join("-").parse::<Locale>() {
            break locale;
        }
        prefix.pop();
    };
    // Legacy POSIX modifiers become variants; ICU keyword assignments are
    // separate and are not a bare variant (e.g. @collation=phonebook).
    if !modifier.is_empty() && !modifier.contains('=') {
        let variant = format!("{locale}-{modifier}");
        locale = variant.parse().unwrap_or_else(|_| {
            format!("{locale}-x-lvariant-{modifier}")
                .parse()
                .unwrap_or(locale)
        });
    }
    locale
}

pub fn node_collator(
    locale: &Locale,
    process_default: bool,
) -> Result<CollatorBorrowed<'static>, String> {
    let mut preferences: CollatorPreferences = locale.clone().into();
    let create = |preferences: CollatorPreferences| {
        Collator::try_new(preferences, Default::default()).map(|collator| {
            // The pinned baked metadata omits fr-CA's backward-secondary bit.
            // Restore the Node/ICU78 default; explicit collations (e.g. emoji)
            // retain their own metadata. See native-collation/oracle.json.
            let latin_or_none = |script: Option<icu_locale_core::subtags::Script>| {
                script.is_none_or(|script| script.to_string() == "Latn")
            };
            if locale.id.language.as_str() == "fr"
                && (process_default && latin_or_none(locale.id.script)
                    || !process_default && locale.id.script.is_none())
                && locale
                    .id
                    .region
                    .is_some_and(|region| region.as_str() == "CA")
                && preferences.collation_type.is_none()
            {
                collator.with_backward_second_level(true)
            } else {
                collator
            }
        })
    };
    create(preferences)
        .or_else(|_| {
            preferences.collation_type = None;
            create(preferences)
        })
        .or_else(|_| Collator::try_new(Default::default(), Default::default()))
        .map_err(|_| "Collation data unavailable.".to_owned())
}

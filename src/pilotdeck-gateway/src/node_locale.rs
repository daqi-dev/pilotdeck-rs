//! Node v22.22.0 / ICU POSIX default locale, measured in node-locale/oracle.json.
use icu_locale_core::Locale;

pub(crate) fn node_locale(env: impl FnMut(&str) -> Option<String>) -> Locale {
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

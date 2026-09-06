pub fn normalize_text(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut last_was_space = false;

    for c in input.to_lowercase().chars() {
        if let Some(folded) = fold_alphanumeric(c) {
            normalized.push(folded);
            last_was_space = false;
        } else if (c.is_whitespace() || matches!(c, ',' | '.' | '-' | '/' | '#' | ':' | ';'))
            && !last_was_space
        {
            normalized.push(' ');
            last_was_space = true;
        }
    }

    normalized.trim().to_string()
}

/// Normalize text for canonical address matching while preserving letters with
/// diacritics. The folded `normalize_text` form is indexed as an additional
/// search alias, so ASCII input can still find accented addresses.
pub fn normalize_text_for_country(_country_code: &str, input: &str) -> String {
    normalize_text_preserving_letters(input)
}

fn normalize_text_preserving_letters(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut last_was_space = false;

    for c in input.to_lowercase().chars() {
        if c.is_alphanumeric() {
            normalized.push(c);
            last_was_space = false;
        } else if (c.is_whitespace() || matches!(c, ',' | '.' | '-' | '/' | '#' | ':' | ';'))
            && !last_was_space
        {
            normalized.push(' ');
            last_was_space = true;
        }
    }

    normalized.trim().to_string()
}

fn fold_alphanumeric(c: char) -> Option<char> {
    let folded = match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ą' => 'a',
        'æ' => 'a',
        'ç' | 'č' | 'ć' => 'c',
        'ď' => 'd',
        'é' | 'ě' | 'è' | 'ê' | 'ë' | 'ę' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ľ' | 'ĺ' | 'ł' => 'l',
        'ñ' | 'ň' => 'n',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
        'ř' => 'r',
        'š' | 'ś' => 's',
        'ť' => 't',
        'ú' | 'ů' | 'ù' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'ž' | 'ź' | 'ż' => 'z',
        _ if c.is_ascii_alphanumeric() => c,
        _ => return None,
    };

    Some(folded)
}

#[cfg(test)]
mod tests {
    use super::{normalize_text, normalize_text_for_country};

    #[test]
    fn normalizes_diacritics_and_punctuation() {
        assert_eq!(
            normalize_text("Avenue de France 123, Stiring-Wendel"),
            "avenue de france 123 stiring wendel"
        );
    }

    #[test]
    fn preserves_slovenian_diacritics() {
        assert_eq!(normalize_text_for_country("SI", "cesta 6č"), "cesta 6č");
        assert_eq!(normalize_text_for_country("SI", "cesta 6c"), "cesta 6c");
        assert_ne!(
            normalize_text_for_country("SI", "cesta 6č"),
            normalize_text_for_country("SI", "cesta 6c")
        );
        assert_eq!(normalize_text_for_country("AT", "Straße"), "straße");
    }
}

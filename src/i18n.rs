//! Public UI language selection. API values, field names and data are unchanged.
use serde::Deserialize;
use xitca_web::http::{WebRequest, WebResponse, header};

#[derive(Default, Deserialize)]
pub struct LanguageQuery {
    lang: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Language {
    Cs,
    Sk,
    En,
}
impl Language {
    fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "en" => Self::En,
            "sk" => Self::Sk,
            _ => Self::Cs,
        }
    }
    pub fn from_request(query: &LanguageQuery, req: &WebRequest<()>) -> Self {
        if let Some(value) = &query.lang {
            return Self::parse(value);
        }
        req.headers()
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|cookies| {
                cookies
                    .split(';')
                    .find_map(|item| item.trim().strip_prefix("addresswise_language="))
            })
            .map(Self::parse)
            .unwrap_or(Self::Cs)
    }
}
macro_rules! page {
    ($name:literal,$lang:expr) => {
        match $lang {
            Language::Cs => include_str!(concat!("../static/cs/", $name, ".html")),
            Language::Sk => include_str!(concat!("../static/sk/", $name, ".html")),
            Language::En => include_str!(concat!("../static/en/", $name, ".html")),
        }
    };
}
pub fn template(name: &str, language: Language) -> &'static str {
    match name {
        "platform" => page!("platform", language),
        "docs" => page!("docs", language),
        "account" => page!("account", language),
        "index" => page!("index", language),
        "admin" => page!("admin", language),
        "terms" => page!("terms", language),
        _ => unreachable!("known UI page"),
    }
}
pub fn html(body: impl Into<String>, language: Language) -> WebResponse {
    let mut response = WebResponse::new(body.into().into_bytes().into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_LANGUAGE,
        header::HeaderValue::from_static(match language {
            Language::Cs => "cs",
            Language::Sk => "sk",
            Language::En => "en",
        }),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-cache"),
    );
    response
        .headers_mut()
        .insert(header::VARY, header::HeaderValue::from_static("Cookie"));
    response
}
pub async fn script() -> WebResponse {
    javascript(include_str!("../static/i18n.js"))
}

pub async fn account_script() -> WebResponse {
    javascript(include_str!("../static/account.js"))
}

fn javascript(source: &str) -> WebResponse {
    let mut response = WebResponse::new(source.as_bytes().to_vec().into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn czech_is_default_and_explicit_language_overrides_cookie() {
        let req = WebRequest::default();
        assert_eq!(
            Language::from_request(&LanguageQuery::default(), &req),
            Language::Cs
        );
        let mut req = WebRequest::default();
        req.headers_mut().insert(
            header::COOKIE,
            header::HeaderValue::from_static("other=1; addresswise_language=sk"),
        );
        assert_eq!(
            Language::from_request(&LanguageQuery::default(), &req),
            Language::Sk
        );
        assert_eq!(
            Language::from_request(
                &LanguageQuery {
                    lang: Some("EN".into())
                },
                &req
            ),
            Language::En
        );
        assert_eq!(Language::parse("cz"), Language::Cs);
        assert_eq!(Language::parse("invalid"), Language::Cs);
    }
}

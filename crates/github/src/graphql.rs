//! GitHub's GraphQL responses, decoded with rozed's own envelope.
//!
//! `graphql_client`'s `Response` drops the `type` GitHub puts on each error (`NOT_FOUND`,
//! `FORBIDDEN`, …), so rozed decodes the envelope itself.

use serde::Deserialize;

use crate::rate_limit::GraphQlRateLimit;

/// One error in a GraphQL response.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct GraphQlError {
    pub message: String,
    /// GitHub's error type, such as `NOT_FOUND` or `FORBIDDEN`. Validation errors have none.
    #[serde(default, rename = "type")]
    pub r#type: Option<String>,
    /// Where in the data the error happened, from the query's root. Empty for an error about
    /// the whole query.
    #[serde(default, deserialize_with = "null_as_empty")]
    pub path: Vec<serde_json::Value>,
    #[serde(default)]
    pub extensions: Option<serde_json::Value>,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<serde_json::Value>, D::Error> {
    Ok(Option::<Vec<serde_json::Value>>::deserialize(deserializer)?.unwrap_or_default())
}

impl GraphQlError {
    /// Whether the error is about a field deeper than `depth` from the root: one GitHub left out
    /// of data it otherwise returned.
    pub fn is_below(&self, depth: usize) -> bool {
        self.path.len() > depth
    }
}

/// A GraphQL response: data, errors, or both when GitHub returns partial data.
#[derive(Debug)]
pub struct GraphQlResponse<T> {
    pub data: Option<T>,
    pub errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    data: Option<serde_json::Value>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

/// Splits a response body into its data, its errors, and the `rateLimit` field if the query
/// asked for one. When the data doesn't fit `T` but there are errors, which is what a field
/// nulled by an error looks like, the errors are returned without the data.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    body: &[u8],
) -> Result<(GraphQlResponse<T>, Option<GraphQlRateLimit>), String> {
    let envelope: Envelope =
        serde_json::from_slice(body).map_err(|error| format!("not a GraphQL response: {error}"))?;
    let rate_limit = envelope
        .data
        .as_ref()
        .and_then(|data| data.get("rateLimit"))
        .and_then(|field| serde_json::from_value(field.clone()).ok());
    let data = match envelope.data {
        Some(serde_json::Value::Null) | None => None,
        Some(data) => match serde_json::from_value(data) {
            Ok(data) => Some(data),
            Err(error) if !envelope.errors.is_empty() => {
                log::debug!("GraphQL data that doesn't fit, explained by its errors: {error}");
                None
            }
            Err(error) => return Err(format!("unexpected data: {error}")),
        },
    };
    Ok((
        GraphQlResponse {
            data,
            errors: envelope.errors,
        },
        rate_limit,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Data {
        repository: Option<serde_json::Value>,
    }

    #[test]
    fn keeps_partial_data_and_typed_errors() {
        let body = br#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","path":["repository"],"locations":[{"line":1,"column":2}],"message":"Could not resolve to a Repository with the name 'rozed-dev/nope'."}]}"#;
        let (response, rate_limit) = decode::<Data>(body).unwrap();
        assert_eq!(response.data, Some(Data { repository: None }));
        assert_eq!(response.errors[0].r#type.as_deref(), Some("NOT_FOUND"));
        assert_eq!(rate_limit, None);
    }

    #[test]
    fn errors_without_data() {
        let body = br#"{"errors":[{"path":["query"],"extensions":{"code":"undefinedField"},"message":"Field 'nope' doesn't exist"}]}"#;
        let (response, _) = decode::<Data>(body).unwrap();
        assert_eq!(response.data, None);
        assert_eq!(response.errors[0].r#type, None);
        assert!(response.errors[0].extensions.is_some());
    }

    #[test]
    fn reads_the_rate_limit_field() {
        let body = br#"{"data":{"rateLimit":{"cost":1,"limit":5000,"remaining":4999,"used":1,"resetAt":"2026-11-06T09:00:00Z"},"repository":{}}}"#;
        let (_, rate_limit) = decode::<Data>(body).unwrap();
        assert_eq!(rate_limit.unwrap().remaining, 4999);
    }

    #[test]
    fn errors_are_kept_when_the_data_doesnt_fit() {
        // A typed error nulls a field the generated types don't expect to be null; the error is
        // what explains it.
        #[derive(Deserialize, Debug)]
        struct Strict {
            #[allow(dead_code)]
            repository: serde_json::Map<String, serde_json::Value>,
        }
        let body = br#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","path":["repository"],"message":"Could not resolve to a Repository"}]}"#;
        let (response, _) = decode::<Strict>(body).unwrap();
        assert!(response.data.is_none());
        assert_eq!(response.errors[0].r#type.as_deref(), Some("NOT_FOUND"));
        // Without errors to explain it, data that doesn't fit is still a decoding error.
        assert!(decode::<Strict>(br#"{"data":{"repository":null}}"#).is_err());
    }

    #[test]
    fn an_error_whose_path_is_null_is_read() {
        let body = br#"{"errors":[{"message":"Something went wrong","path":null}]}"#;
        let (response, _) = decode::<Data>(body).unwrap();
        assert_eq!(response.errors[0].path, Vec::<serde_json::Value>::new());
    }

    #[test]
    fn rejects_a_body_that_is_not_graphql() {
        assert!(decode::<Data>(b"<html>").is_err());
    }
}

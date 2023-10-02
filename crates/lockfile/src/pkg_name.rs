use derive_more::{Display, Error};
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};
use split_first_char::SplitFirstChar;
use std::{fmt, str::FromStr};

/// Represent the name of an npm package.
///
/// Syntax:
/// * Without scope: `{name}`
/// * With scope: `@{scope}/name`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "&'de str", into = "String")]
pub struct PkgName {
    /// Name of the scope (if any) without the `@` prefix.
    pub scope: Option<String>,
    /// Name of the package.
    pub name: String,
}

/// Error when parsing [`PkgName`] from a string input.
#[derive(Debug, Display, Error)]
pub enum ParsePkgNameError {
    #[display(fmt = "Missing name part of the scoped package")]
    MissingName,
    #[display(fmt = "Name is empty")]
    EmptyName,
}

impl PkgName {
    /// Parse [`PkgName`] from a string input.
    pub fn parse<Input>(input: Input) -> Result<Self, ParsePkgNameError>
    where
        Input: Into<String> + AsRef<str>,
    {
        match input.as_ref().split_first_char() {
            Some(('@', rest)) => {
                let (scope, name) = rest.split_once('/').ok_or(ParsePkgNameError::MissingName)?;
                let scope = scope.to_string().pipe(Some);
                let name = name.to_string();
                Ok(PkgName { scope, name })
            }
            Some(_) => {
                let scope = None;
                let name = input.into();
                Ok(PkgName { scope, name })
            }
            None => Err(ParsePkgNameError::EmptyName),
        }
    }
}

impl TryFrom<String> for PkgName {
    type Error = ParsePkgNameError;
    fn try_from(input: String) -> Result<Self, Self::Error> {
        PkgName::parse(input)
    }
}

impl<'a> TryFrom<&'a str> for PkgName {
    type Error = ParsePkgNameError;
    fn try_from(input: &'a str) -> Result<Self, Self::Error> {
        PkgName::parse(input)
    }
}

impl FromStr for PkgName {
    type Err = ParsePkgNameError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        PkgName::parse(input)
    }
}

impl fmt::Display for PkgName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let PkgName { scope, name } = self;
        if let Some(scope) = scope {
            write!(f, "@{scope}/")?;
        }
        write!(f, "{name}")
    }
}

impl From<PkgName> for String {
    fn from(value: PkgName) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_ok() {
        fn case(input: &'static str, output: PkgName) {
            eprintln!("CASE: {input:?}");
            let actual: PkgName = input.parse().unwrap();
            assert_eq!(&actual, &output);
        }

        case("@foo/bar", PkgName { scope: Some("foo".to_string()), name: "bar".to_string() });
        case("foo-bar", PkgName { scope: None, name: "foo-bar".to_string() });
    }

    #[test]
    fn deserialize_ok() {
        fn case(input: &'static str, output: PkgName) {
            eprintln!("CASE: {input:?}");
            let actual: PkgName = serde_yaml::from_str(input).unwrap();
            assert_eq!(&actual, &output);
        }

        case("'@foo/bar'", PkgName { scope: Some("foo".to_string()), name: "bar".to_string() });
        case("foo-bar", PkgName { scope: None, name: "foo-bar".to_string() });
    }

    #[test]
    fn parse_err() {
        macro_rules! case {
            ($input:expr => $message:expr, $pattern:pat) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let error = input.parse::<PkgName>().unwrap_err();
                dbg!(&error);
                assert_eq!(error.to_string(), $message);
                assert!(matches!(&error, $pattern));
            }};
        }

        case!("@foo" => "Missing name part of the scoped package", ParsePkgNameError::MissingName);
        case!("" => "Name is empty", ParsePkgNameError::EmptyName);
    }

    #[test]
    fn to_string() {
        fn case(input: PkgName, output: &'static str) {
            eprintln!("CASE: {input:?}");
            assert_eq!(input.to_string(), output);
        }

        case(PkgName { scope: Some("foo".to_string()), name: "bar".to_string() }, "@foo/bar");
        case(PkgName { scope: None, name: "foo-bar".to_string() }, "foo-bar");
    }

    #[test]
    fn serialize() {
        fn case(input: PkgName, output: &'static str) {
            eprintln!("CASE: {input:?}");
            let received = serde_yaml::to_value(&input).unwrap();
            let expected = output.to_string().pipe(serde_yaml::Value::String);
            assert_eq!(&received, &expected);
        }

        case(PkgName { scope: Some("foo".to_string()), name: "bar".to_string() }, "@foo/bar");
        case(PkgName { scope: None, name: "foo-bar".to_string() }, "foo-bar");
    }
}

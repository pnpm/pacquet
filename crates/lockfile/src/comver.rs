use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use std::{num::ParseIntError, str::FromStr};

/// Version struct used in a lockfile.
///
/// It contains only major and minor.
#[derive(Debug, Display, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[display(fmt = "{major}.{minor}")]
#[serde(try_from = "String", into = "String")]
pub struct ComVer {
    pub major: u16,
    pub minor: u16,
}

impl ComVer {
    /// Create a comver struct.
    pub fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

/// Error when parsing [`ComVer`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParseComVerError {
    #[display(fmt = "Dot is missing")]
    MissingDot,
    #[display(fmt = "Major is not a valid number: {_0}")]
    InvalidMajor(ParseIntError),
    #[display(fmt = "Minor is not a valid number: {_0}")]
    InvalidMinor(ParseIntError),
}

impl FromStr for ComVer {
    type Err = ParseComVerError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (major, minor) = s.split_once('.').ok_or(ParseComVerError::MissingDot)?;
        let major = major.parse::<u16>().map_err(ParseComVerError::InvalidMajor)?;
        let minor = minor.parse::<u16>().map_err(ParseComVerError::InvalidMinor)?;
        Ok(ComVer::new(major, minor))
    }
}

impl TryFrom<String> for ComVer {
    type Error = ParseComVerError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ComVer> for String {
    fn from(value: ComVer) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse() {
        assert_eq!("6.0".parse::<ComVer>().unwrap(), ComVer::new(6, 0));
    }

    #[test]
    fn to_string() {
        assert_eq!(ComVer::new(6, 0).to_string(), "6.0");
    }
}

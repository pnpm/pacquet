#[derive(Debug, PartialEq)]
pub enum VersionPin {
    None,
    Patch,
    Minor,
    Major,
}

/// @see https://github.com/pnpm/pnpm/blob/main/packages/which-version-is-pinned/src/index.ts#L3
pub fn parse_version(input: &str) -> (VersionPin, &str) {
    let mut version = input;

    match version.chars().nth(0) {
        Some('~') => (VersionPin::Minor, &version[1..]),
        Some('^') => (VersionPin::Major, &version[1..]),
        _ => (VersionPin::None, version),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_version_parse() {
        assert_eq!(parse_version("~1.0.0"), (VersionPin::Minor, "1.0.0"));
        assert_eq!(parse_version("^1.0.0"), (VersionPin::Major, "1.0.0"));
        assert_eq!(parse_version("1.0.0"), (VersionPin::None, "1.0.0"));
    }
}

#[derive(Debug, PartialEq)]
pub enum VersionPin {
    None,
    Patch,
    Minor,
    Major,
}

/// @see https://github.com/pnpm/pnpm/blob/main/packages/which-version-is-pinned/src/index.ts#L3
pub fn get_version_pin(input: &str) -> VersionPin {
    let mut starting_character = 0;

    if input.starts_with("workspace:") {
        if input == "workspace:*" {
            return VersionPin::Patch;
        }

        starting_character = 10;
    } else if input.starts_with("npm:") {
        if let Some(index) = input.rfind('@') {
            starting_character = index + 1;
        } else {
            starting_character = 4;
        }
    } else if input == "*" {
        return VersionPin::None;
    }

    if let Some(starting) = input.chars().nth(starting_character) {
        return match starting {
            '~' => VersionPin::Minor,
            '^' => VersionPin::Major,
            _ => VersionPin::None,
        };
    }

    VersionPin::None
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_version_pin() {
        assert_eq!(get_version_pin("workspace:*"), VersionPin::Patch);
        assert_eq!(get_version_pin("workspace:~1.0.0"), VersionPin::Minor);
        assert_eq!(get_version_pin("workspace:hello/*"), VersionPin::None);
        assert_eq!(get_version_pin("npm:fast-querystring"), VersionPin::None);
        assert_eq!(get_version_pin("npm:fast-querystring@1.0.0"), VersionPin::None);
        assert_eq!(get_version_pin("npm:fast-querystring@^1.0.0"), VersionPin::Major);
        assert_eq!(get_version_pin("npm:fast-querystring@~1.0.0"), VersionPin::Minor);
        assert_eq!(get_version_pin("~1.0.0"), VersionPin::Minor);
        assert_eq!(get_version_pin("^1.0.0"), VersionPin::Major);
        assert_eq!(get_version_pin("1.0.0"), VersionPin::None);
    }
}

use super::{ParsePkgNameError, PkgName};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;

#[test]
fn parse_ok() {
    fn case(input: &'static str, output: PkgName) {
        eprintln!("CASE: {input:?}");
        let actual: PkgName = input.parse().unwrap();
        assert_eq!(&actual, &output);
    }

    case("@foo/bar", PkgName { scope: Some("foo".to_string()), bare: "bar".to_string() });
    case("foo-bar", PkgName { scope: None, bare: "foo-bar".to_string() });
}

#[test]
fn deserialize_ok() {
    fn case(input: &'static str, output: PkgName) {
        eprintln!("CASE: {input:?}");
        let actual: PkgName = serde_saphyr::from_str(input).unwrap();
        assert_eq!(&actual, &output);
    }

    case("'@foo/bar'", PkgName { scope: Some("foo".to_string()), bare: "bar".to_string() });
    case("foo-bar", PkgName { scope: None, bare: "foo-bar".to_string() });
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

    case!("@foo" => "Missing bare name", ParsePkgNameError::MissingName);
    case!("" => "Name is empty", ParsePkgNameError::EmptyName);
}

#[test]
fn to_string() {
    fn case(input: PkgName, output: &'static str) {
        eprintln!("CASE: {input:?}");
        assert_eq!(input.to_string(), output);
    }

    case(PkgName { scope: Some("foo".to_string()), bare: "bar".to_string() }, "@foo/bar");
    case(PkgName { scope: None, bare: "foo-bar".to_string() }, "foo-bar");
}

#[test]
fn serialize() {
    fn case(input: PkgName, output: &'static str) {
        eprintln!("CASE: {input:?}");
        let received = input.pipe_ref(serde_saphyr::to_string).unwrap();
        assert_eq!(received, output);
    }

    case(PkgName { scope: Some("foo".to_string()), bare: "bar".to_string() }, "\"@foo/bar\"\n");
    case(PkgName { scope: None, bare: "foo-bar".to_string() }, "foo-bar\n");
}

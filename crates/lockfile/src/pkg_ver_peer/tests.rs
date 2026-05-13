use super::{ParsePkgVerPeerError, PkgVerPeer};
use node_semver::Version;
use pretty_assertions::assert_eq;

fn assert_ver_peer<Ver, Peer>(received: PkgVerPeer, expected_version: Ver, expected_peer: Peer)
where
    Ver: Into<Version>,
    Peer: Into<String>,
{
    dbg!(&received);
    let expected_version = expected_version.into();
    let expected_peer = expected_peer.into();
    assert_eq!((received.version(), received.peer()), (&expected_version, expected_peer.as_str()));
    assert_eq!(received.into_tuple(), (expected_version, expected_peer));
}

fn decode_encode_case<Decode, Encode>(input: &str, decode: Decode, encode: Encode)
where
    Decode: Fn(&str) -> PkgVerPeer,
    Encode: Fn(&PkgVerPeer) -> String,
{
    eprintln!("CASE: {input:?}");
    let peer_ver = decode(input);
    dbg!(&peer_ver);
    let output = encode(&peer_ver);
    assert_eq!(input, output);
}

#[test]
fn parse_ok() {
    fn case<Ver, Peer>(input: &'static str, (expected_version, expected_peer): (Ver, Peer))
    where
        Ver: Into<Version>,
        Peer: Into<String>,
    {
        eprintln!("CASE: {input:?}");
        assert_ver_peer(input.parse().unwrap(), expected_version, expected_peer);
    }

    case(
        "1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)",
        ((1, 21, 3), "(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)"),
    );
    case("1.21.3(react@17.0.2)", ((1, 21, 3), "(react@17.0.2)"));
    case(
        "1.21.3-rc.0(react@17.0.2)",
        ("1.21.3-rc.0".parse::<Version>().unwrap(), "(react@17.0.2)"),
    );
    case("1.21.3", ((1, 21, 3), ""));
    case("1.21.3-rc.0", ("1.21.3-rc.0".parse::<Version>().unwrap(), ""));
}

#[test]
fn parse_err() {
    macro_rules! case {
        ($input:expr => $message:expr, $variant:pat) => {{
            let input = $input;
            eprintln!("CASE: {input:?}");
            let error = input.parse::<PkgVerPeer>().unwrap_err();
            dbg!(&error);
            assert_eq!(error.to_string(), $message);
            assert!(matches!(error, $variant));
        }};
    }
    case!("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2" => "Mismatch parenthesis", ParsePkgVerPeerError::MismatchParenthesis);
    case!("1.21.3(" => "Mismatch parenthesis", ParsePkgVerPeerError::MismatchParenthesis);
    case!("1.21.3)" => "Mismatch parenthesis", ParsePkgVerPeerError::MismatchParenthesis);
    case!("a.b.c" => "Failed to parse the version part: Failed to parse version.", ParsePkgVerPeerError::ParseVersionFailure(_));
}

#[test]
fn deserialize_ok() {
    fn case<Ver, Peer>(input: &'static str, (expected_version, expected_peer): (Ver, Peer))
    where
        Ver: Into<Version>,
        Peer: Into<String>,
    {
        eprintln!("CASE: {input:?}");
        assert_ver_peer(serde_saphyr::from_str(input).unwrap(), expected_version, expected_peer);
    }

    case(
        "1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)",
        ((1, 21, 3), "(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)"),
    );
    case("1.21.3(react@17.0.2)", ((1, 21, 3), "(react@17.0.2)"));
    case(
        "1.21.3-rc.0(react@17.0.2)",
        ("1.21.3-rc.0".parse::<Version>().unwrap(), "(react@17.0.2)"),
    );
    case("1.21.3", ((1, 21, 3), ""));
    case("1.21.3-rc.0", ("1.21.3-rc.0".parse::<Version>().unwrap(), ""));
}

#[test]
fn parse_to_string() {
    let case =
        |input| decode_encode_case(input, |input| input.parse().unwrap(), ToString::to_string);
    case("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
    case("1.21.3(react@17.0.2)");
    case("1.21.3-rc.0(react@17.0.2)");
    case("1.21.3");
    case("1.21.3-rc.0");
}

#[test]
fn deserialize_serialize() {
    let case = |input| {
        decode_encode_case(
            input,
            |input| serde_saphyr::from_str(input).unwrap(),
            |ver_peer| serde_saphyr::to_string(&ver_peer).unwrap().trim().to_string(),
        )
    };
    case("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
    case("1.21.3(react@17.0.2)");
    case("1.21.3-rc.0(react@17.0.2)");
    case("1.21.3");
    case("1.21.3-rc.0");
}

// ---------------------------------------------------------------------------
// `runtime:` scheme prefix (#511 / #437 §F unblocker)
// ---------------------------------------------------------------------------

/// `runtime:22.0.0` parses with the prefix recorded separately
/// from the version. The version part is the bare semver, the
/// peer part is empty.
#[test]
fn parse_runtime_prefix_without_peer() {
    let parsed: PkgVerPeer = "runtime:22.0.0".parse().expect("runtime: prefix must parse");
    assert_eq!(parsed.prefix(), Some("runtime:"));
    assert_eq!(parsed.version(), &Version::from((22, 0, 0)));
    assert_eq!(parsed.peer(), "");
    assert!(parsed.is_runtime());
}

/// Runtime entries can in principle carry a peer suffix too
/// (no upstream example today, but the grammar admits it). The
/// prefix doesn't disable the parenthesis handling.
#[test]
fn parse_runtime_prefix_with_peer() {
    let parsed: PkgVerPeer =
        "runtime:1.0.0(some-peer@1.0.0)".parse().expect("runtime:+peer must parse");
    assert_eq!(parsed.prefix(), Some("runtime:"));
    assert_eq!(parsed.version(), &Version::from((1, 0, 0)));
    assert_eq!(parsed.peer(), "(some-peer@1.0.0)");
    assert!(parsed.is_runtime());
}

/// Plain semver still parses with `prefix() == None`. Baseline
/// sanity to ensure the new branch doesn't false-trigger.
#[test]
fn parse_plain_semver_has_no_prefix() {
    let parsed: PkgVerPeer = "1.21.3".parse().unwrap();
    assert_eq!(parsed.prefix(), None);
    assert!(!parsed.is_runtime());
}

/// Display round-trip: `runtime:22.0.0` parses then displays to
/// the same byte string. Required so `PackageKey::to_string()`
/// produces the same depPath the lockfile uses.
#[test]
fn runtime_prefix_round_trips_through_display() {
    for input in
        ["runtime:22.0.0", "runtime:1.0.0-beta.1", "runtime:1.0.0(some-peer@1.0.0)", "1.21.3"]
    {
        let parsed: PkgVerPeer = input.parse().expect("parse");
        let displayed = parsed.to_string();
        assert_eq!(displayed, input, "round-trip mismatch for {input:?}");
    }
}

/// Other URL-style prefixes (`tag:` etc.) aren't recognised yet
/// — only `runtime:` is. A `tag:` input parses as a plain
/// version-with-bad-format and errors out. Per #511's "Out of
/// scope": land `runtime:` first, generalize later if needed.
#[test]
fn other_scheme_prefixes_are_not_recognised() {
    let err = "tag:22.0.0".parse::<PkgVerPeer>().expect_err("tag: prefix must not parse yet");
    assert!(
        matches!(err, ParsePkgVerPeerError::ParseVersionFailure(_)),
        "expected ParseVersionFailure for unrecognised scheme, got {err:?}",
    );
}

/// `runtime:` works at the `PackageKey` level too — i.e. via
/// `PkgNameVerPeer::parse`. The bug the parent issue calls out
/// is that `"node@runtime:22.0.0".parse::<PackageKey>()` errors;
/// after this fix it round-trips.
#[test]
fn package_key_runtime_round_trip() {
    use crate::PackageKey;
    let parsed: PackageKey =
        "node@runtime:22.0.0".parse().expect("PackageKey must accept runtime: depPaths");
    assert_eq!(parsed.to_string(), "node@runtime:22.0.0");
    assert!(parsed.suffix.is_runtime());
}

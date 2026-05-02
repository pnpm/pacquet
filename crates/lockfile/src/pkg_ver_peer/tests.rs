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
    assert_eq!((received.version(), received.peer()), (&expected_version, expected_peer.as_str()),);
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

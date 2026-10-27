//! Distinct original artifact kinds from the same exact official release.
//! Existing verified CLI is the intended product path; standalone is a CI reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageKind {
    Cli,
    #[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
    Standalone,
}
impl ImageKind {
    #[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
    pub(crate) fn bytes(self) -> u64 {
        match self {
            Self::Cli => 354_558_768,
            Self::Standalone => crate::IMAGE_BYTES,
        }
    }
    pub(crate) fn sha256(self) -> &'static str {
        match self {
            Self::Cli => "6f267830b029ee25d9339864aa867cd45de9f8fd4c5ea007d2e63c13a0c66d36",
            #[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
            Self::Standalone => crate::IMAGE_SHA256,
        }
    }
    pub(crate) fn args(self) -> &'static [&'static str] {
        match self {
            Self::Cli => &["app-server", "--strict-config", "--listen", "stdio://"],
            #[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
            Self::Standalone => crate::config::APP_SERVER_ARGS,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_kinds_do_not_share_an_argv_or_identity() {
        assert_ne!(ImageKind::Cli.bytes(), ImageKind::Standalone.bytes());
        assert_ne!(ImageKind::Cli.sha256(), ImageKind::Standalone.sha256());
        assert_eq!(ImageKind::Cli.args()[0], "app-server");
        assert_eq!(&ImageKind::Cli.args()[1..], ImageKind::Standalone.args());
        for kind in [ImageKind::Cli, ImageKind::Standalone] {
            assert_eq!(kind.sha256().len(), 64);
            assert!(kind.sha256().bytes().all(|c| c.is_ascii_hexdigit()));
        }
    }
}

/// Immutable offline reference pair from original official release assets.
/// Runtime version text is not authority; complete held bytes must match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewedCli {
    V163,
    V164,
}
impl ReviewedCli {
    #[cfg(any(test, all(windows, debug_assertions, feature = "native-fixtures")))]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "0.163.0" => Some(Self::V163),
            "0.164.0" => Some(Self::V164),
            _ => None,
        }
    }
    pub(crate) fn version(self) -> &'static str {
        match self {
            Self::V163 => "0.163.0",
            Self::V164 => "0.164.0",
        }
    }
    pub(crate) fn bytes(self) -> u64 {
        match self {
            Self::V163 => 341_270_320,
            Self::V164 => 354_558_768,
        }
    }
    pub(crate) fn sha256(self) -> &'static str {
        match self {
            Self::V163 => "2692c8c7bb2cf8b0aefe638741fc90236f4b824c59578ce84994f9de58200993",
            Self::V164 => ImageKind::Cli.sha256(),
        }
    }
    pub(crate) fn recognize(bytes: u64, digest: &[u8; 32]) -> Option<Self> {
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        [Self::V163, Self::V164]
            .into_iter()
            .find(|pin| bytes == pin.bytes() && hex == pin.sha256())
    }
}
#[cfg(test)]
mod reviewed_tests {
    use super::*;
    #[test]
    fn reviewed_pair_requires_all_bytes_and_has_no_version_only_admission() {
        for pin in [ReviewedCli::V163, ReviewedCli::V164] {
            let mut digest = [0; 32];
            for (i, byte) in digest.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&pin.sha256()[i * 2..i * 2 + 2], 16).unwrap();
            }
            assert_eq!(ReviewedCli::recognize(pin.bytes(), &digest), Some(pin));
            assert_eq!(ReviewedCli::parse(pin.version()), Some(pin));
            assert_eq!(ReviewedCli::recognize(pin.bytes() + 1, &digest), None);
            digest[0] ^= 1;
            assert_eq!(ReviewedCli::recognize(pin.bytes(), &digest), None);
        }
        assert!(ReviewedCli::parse("0.165.0").is_none());
    }
}

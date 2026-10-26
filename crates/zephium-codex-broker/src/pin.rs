//! Distinct original artifact kinds from the same exact official release.
//! Existing verified CLI is the intended product path; standalone is a CI reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageKind {
    Cli,
    Standalone,
}
impl ImageKind {
    pub(crate) fn bytes(self) -> u64 {
        match self {
            Self::Cli => 354_558_768,
            Self::Standalone => crate::IMAGE_BYTES,
        }
    }
    pub(crate) fn sha256(self) -> &'static str {
        match self {
            Self::Cli => "6f267830b029ee25d9339864aa867cd45de9f8fd4c5ea007d2e63c13a0c66d36",
            Self::Standalone => crate::IMAGE_SHA256,
        }
    }
    pub(crate) fn args(self) -> &'static [&'static str] {
        match self {
            Self::Cli => &["app-server", "--strict-config", "--listen", "stdio://"],
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

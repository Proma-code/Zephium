//! Fixed locator vocabulary; none of these paths or metadata authorize execution.
use serde::Deserialize;

pub(crate) const MAX_METADATA: usize = 64 * 1024;
pub(crate) const MAX_PATH_HINTS: usize = 64;
pub(crate) const MAX_PATH_UNITS: usize = 32_767;
pub(crate) const MAX_FULL_HASHES: usize = 8;
pub(crate) const NPM_ROOT: &[&str] = &["node_modules", "@openai", "codex"];
pub(crate) const PLATFORM_NESTED: &[&str] = &[
    "node_modules",
    "@openai",
    "codex",
    "node_modules",
    "@openai",
    "codex-win32-x64",
];
pub(crate) const PLATFORM_HOISTED: &[&str] = &["node_modules", "@openai", "codex-win32-x64"];
pub(crate) const VENDOR_IMAGE: &[&str] = &["vendor", "x86_64-pc-windows-msvc", "bin", "codex.exe"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataFault {
    Shape,
    Version,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HintFault {
    TooLong,
    TooMany,
}

/// No shell interpretation, environment expansion, CWD, UNC or device search.
/// Individual malformed entries are skipped; the complete input stays bounded.
pub(crate) fn path_hints(input: &str) -> Result<Vec<&str>, HintFault> {
    if input.encode_utf16().count() > MAX_PATH_UNITS {
        return Err(HintFault::TooLong);
    }
    let mut result = Vec::new();
    for (index, part) in input.split(';').enumerate() {
        if index >= MAX_PATH_HINTS {
            return Err(HintFault::TooMany);
        }
        if drive_absolute(part)
            && !result
                .iter()
                .any(|prior: &&str| prior.eq_ignore_ascii_case(part))
        {
            result.push(part);
        }
    }
    Ok(result)
}
fn drive_absolute(input: &str) -> bool {
    let bytes = input.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1..3] != *b":\\" {
        return false;
    }
    let tail = &input[3..];
    if tail.is_empty() {
        return true;
    }
    tail.split('\\').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with([' ', '.'])
            && !part.chars().any(|c| {
                c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*')
            })
    })
}

#[derive(Deserialize)]
struct RootPackage {
    name: String,
    version: String,
    bin: BinEntry,
    #[serde(rename = "optionalDependencies")]
    optional_dependencies: PlatformAlias,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinEntry {
    codex: String,
}
#[derive(Deserialize)]
struct PlatformAlias {
    #[serde(rename = "@openai/codex-win32-x64")]
    windows_x64: String,
}
#[derive(Deserialize)]
struct PlatformPackage {
    name: String,
    version: String,
    os: Vec<String>,
    cpu: Vec<String>,
}

pub(crate) fn root_package(bytes: &[u8]) -> Result<String, MetadataFault> {
    if bytes.len() > MAX_METADATA {
        return Err(MetadataFault::Shape);
    }
    let value: RootPackage = serde_json::from_slice(bytes).map_err(|_| MetadataFault::Shape)?;
    if value.name != "@openai/codex" || value.bin.codex != "bin/codex.js" {
        return Err(MetadataFault::Shape);
    }
    crate::release_admission::ReleaseQuery::from_version_hint(&value.version)
        .map_err(|_| MetadataFault::Version)?;
    let expected = format!("npm:@openai/codex@{}-win32-x64", value.version);
    if value.optional_dependencies.windows_x64 != expected {
        return Err(MetadataFault::Version);
    }
    Ok(value.version)
}
pub(crate) fn platform_package(bytes: &[u8]) -> Result<String, MetadataFault> {
    if bytes.len() > MAX_METADATA {
        return Err(MetadataFault::Shape);
    }
    let value: PlatformPackage = serde_json::from_slice(bytes).map_err(|_| MetadataFault::Shape)?;
    // The alias directory is NOT the package's actual published name.
    if value.name != "@openai/codex" || value.os != ["win32"] || value.cpu != ["x64"] {
        return Err(MetadataFault::Shape);
    }
    let version = value
        .version
        .strip_suffix("-win32-x64")
        .ok_or(MetadataFault::Version)?;
    crate::release_admission::ReleaseQuery::from_version_hint(version)
        .map_err(|_| MetadataFault::Version)?;
    Ok(version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> serde_json::Value {
        serde_json::json!({"name":"@openai/codex", "version":crate::RELEASE,
        "bin":{"codex":"bin/codex.js"}, "optionalDependencies":{"@openai/codex-win32-x64":format!("npm:@openai/codex@{}-win32-x64",crate::RELEASE)}})
    }
    #[test]
    fn path_hints_are_finite_absolute_local_non_shell_data() {
        assert_eq!(
            path_hints(
                r";relative;C:relative;\\host\share;\\?\C:\device;\rooted;C:\safe;C:\safe;D:\other"
            )
            .unwrap(),
            [r"C:\safe", r"D:\other"]
        );
        for bad in [
            r"C:\a\..\b",
            r"C:\a\.\b",
            r#""C:\safe""#,
            r"C:\a:stream",
            r"C:\%PATH%\*",
            "C:\\bad\npath",
            r"C:\a.\b",
            r"C:\a ",
        ] {
            assert!(path_hints(bad).unwrap().is_empty(), "{bad}");
        }
        assert!(matches!(
            path_hints(&";".repeat(MAX_PATH_HINTS)),
            Err(HintFault::TooMany)
        ));
        assert!(matches!(
            path_hints(&"x".repeat(MAX_PATH_UNITS + 1)),
            Err(HintFault::TooLong)
        ));
    }
    #[test]
    fn fixed_candidate_suffixes_never_reference_wrappers_or_auth() {
        for parts in [NPM_ROOT, PLATFORM_NESTED, PLATFORM_HOISTED, VENDOR_IMAGE] {
            for part in parts {
                assert!(!part.is_empty() && *part != ".." && !part.contains(['\\', '/', ':']));
            }
        }
        assert_eq!(VENDOR_IMAGE.last(), Some(&"codex.exe"));
        assert_eq!(MAX_FULL_HASHES, 8);
    }
    #[test]
    fn npm_alias_metadata_uses_actual_package_name_and_exact_pin() {
        assert_eq!(
            root_package(&serde_json::to_vec(&root()).unwrap()),
            Ok(crate::RELEASE.to_owned())
        );
        let mut value = root();
        value["version"] = "0.163.0".into();
        assert_eq!(
            root_package(&serde_json::to_vec(&value).unwrap()),
            Err(MetadataFault::Version)
        );
        let mut value = root();
        value["bin"]["codex"] = "../../script.js".into();
        assert_eq!(
            root_package(&serde_json::to_vec(&value).unwrap()),
            Err(MetadataFault::Shape)
        );
        let platform = serde_json::json!({"name":"@openai/codex","version":format!("{}-win32-x64",crate::RELEASE),"os":["win32"],"cpu":["x64"]});
        assert_eq!(
            platform_package(&serde_json::to_vec(&platform).unwrap()),
            Ok(crate::RELEASE.to_owned())
        );
        let mut wrong = platform;
        wrong["name"] = "@openai/codex-win32-x64".into();
        assert_eq!(
            platform_package(&serde_json::to_vec(&wrong).unwrap()),
            Err(MetadataFault::Shape)
        );
    }
    #[test]
    fn stable_future_version_is_only_a_fixed_origin_locator_hint() {
        for version in ["0.163.0", "0.165.0", "1.2.3"] {
            let mut value = root();
            value["version"] = version.into();
            value["optionalDependencies"]["@openai/codex-win32-x64"] =
                format!("npm:@openai/codex@{version}-win32-x64").into();
            assert_eq!(
                root_package(&serde_json::to_vec(&value).unwrap()),
                Ok(version.into())
            );
        }
        for version in ["../../bad", "0.165.0-beta", "1.2.3/extra", "01.2.3"] {
            let mut value = root();
            value["version"] = version.into();
            assert!(root_package(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    #[test]
    fn metadata_never_executes_and_rejects_ambiguous_known_fields_and_size() {
        assert_eq!(
            root_package(br#"{"name":"@openai/codex","name":"other"}"#),
            Err(MetadataFault::Shape)
        );
        assert_eq!(platform_package(br#"{"name":"@openai/codex","version":"0.164.0-win32-x64","os":["win32"],"cpu":["arm64"]}"#),Err(MetadataFault::Shape));
        assert_eq!(
            root_package(&vec![b' '; MAX_METADATA + 1]),
            Err(MetadataFault::Shape)
        );
    }
}

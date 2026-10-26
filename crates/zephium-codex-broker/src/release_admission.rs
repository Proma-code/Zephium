//! Fixed-origin metadata for original, stable Codex Windows x64 CLI images.
//!
//! No network, download, process, account or model operation occurs here. A
//! version hint only selects a public release lookup; it never authenticates a
//! local executable. The native HTTP owner must obtain the response over normal
//! verified HTTPS, without redirects, credentials or cookies, and enforce the
//! streaming body/deadline bounds before issuing `OfficialReleaseResponse`.
//! Parsing arbitrary JSON is not origin authentication. The resulting descriptor
//! is not execution authority: the native image owner must compare its complete
//! held file's size and SHA-256, then independently establish required protocol
//! and capability compatibility. Version never changes profile/vault identity.

use std::time::{Duration, Instant};

use serde::Deserialize;

const API_ROOT: &str = "https://api.github.com/repos/openai/codex/releases";
const RELEASE_ROOT: &str = "https://github.com/openai/codex/releases";
const CLI_NAME: &str = "codex-x86_64-pc-windows-msvc.exe";
pub(crate) const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_INDEX_METADATA_BYTES: usize = 8 * 1024 * 1024;
const MAX_DISCOVERY_METADATA_BYTES: usize = 24 * 1024 * 1024;
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_MAX_AGE: Duration = Duration::from_secs(60);
const MAX_ASSETS: usize = 512;
const MAX_IMAGE_BYTES: u64 = 1024 * 1024 * 1024;
const INDEX_PAGE_SIZE: usize = 10;
const INDEX_PAGES: u8 = 2;
const MAX_LOOKUPS: usize = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    VersionHint,
    Origin,
    HttpStatus,
    Time,
    Bounds,
    Metadata,
    Release,
    Asset,
    Digest,
}

/// Native lookup plan, not a version-specific execution allowlist.
#[derive(Debug)]
pub(crate) struct ReleaseQuery {
    url: String,
    version: Option<String>,
    index: bool,
}

impl ReleaseQuery {
    pub(crate) fn latest() -> Self {
        Self {
            url: format!("{API_ROOT}/latest"),
            version: None,
            index: false,
        }
    }

    /// A package/file version is merely a locator for fixed-origin metadata.
    pub(crate) fn from_version_hint(version: &str) -> Result<Self, Fault> {
        validate_version(version)?;
        Ok(Self {
            url: format!("{API_ROOT}/tags/rust-v{version}"),
            version: Some(version.to_owned()),
            index: false,
        })
    }

    pub(crate) fn recent(page: u8) -> Result<Self, Fault> {
        if !(1..=INDEX_PAGES).contains(&page) {
            return Err(Fault::Bounds);
        }
        Ok(Self {
            url: format!("{API_ROOT}?per_page={INDEX_PAGE_SIZE}&page={page}"),
            version: None,
            index: true,
        })
    }

    pub(crate) fn max_metadata_bytes(&self) -> usize {
        if self.index {
            MAX_INDEX_METADATA_BYTES
        } else {
            MAX_METADATA_BYTES
        }
    }

    pub(crate) fn is_index(&self) -> bool {
        self.index
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

/// Move-only receipt from the trusted native HTTP transport. It deliberately
/// has no serde constructor and must never be issued from renderer/provider JSON.
pub(crate) struct OfficialReleaseResponse {
    query: ReleaseQuery,
    completed: Instant,
    body: Vec<u8>,
}

impl OfficialReleaseResponse {
    /// Native HTTP owner only. That owner disables redirects and authentication,
    /// validates TLS and applies these same bounds during the actual read. URL
    /// equality here does not establish TLS/origin by itself.
    pub(crate) fn from_native_fetch(
        query: ReleaseQuery,
        final_url: &str,
        status: u16,
        started: Instant,
        completed: Instant,
        body: Vec<u8>,
    ) -> Result<Self, Fault> {
        if final_url != query.url() {
            return Err(Fault::Origin);
        }
        if status != 200 {
            return Err(Fault::HttpStatus);
        }
        let elapsed = completed
            .checked_duration_since(started)
            .ok_or(Fault::Time)?;
        if elapsed > FETCH_TIMEOUT {
            return Err(Fault::Time);
        }
        if body.is_empty() || body.len() > query.max_metadata_bytes() {
            return Err(Fault::Bounds);
        }
        Ok(Self {
            query,
            completed,
            body,
        })
    }

    pub(crate) fn decode(self, now: Instant) -> Result<OriginalCliArtifact, Fault> {
        let age = now
            .checked_duration_since(self.completed)
            .ok_or(Fault::Time)?;
        if age > RESPONSE_MAX_AGE {
            return Err(Fault::Time);
        }
        if self.query.is_index() {
            return Err(Fault::Metadata);
        }
        decode_metadata(self.query, &self.body)
    }

    pub(crate) fn decode_index(self, now: Instant) -> Result<Vec<OriginalCliArtifact>, Fault> {
        let age = now
            .checked_duration_since(self.completed)
            .ok_or(Fault::Time)?;
        if age > RESPONSE_MAX_AGE {
            return Err(Fault::Time);
        }
        if !self.query.is_index() {
            return Err(Fault::Metadata);
        }
        // Deserialize directly to typed rows so duplicate known fields do not
        // disappear in a generic JSON round-trip. Every row counts toward the
        // bound, including pre-releases and unrelated asset-only releases.
        let rows: Vec<ReleaseMetadata> =
            serde_json::from_slice(&self.body).map_err(|_| Fault::Metadata)?;
        if rows.len() > INDEX_PAGE_SIZE {
            return Err(Fault::Bounds);
        }
        let mut result = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            if !seen.insert(row.id) {
                return Err(Fault::Release);
            }
            if row.draft
                || row.prerelease
                || row
                    .tag_name
                    .strip_prefix("rust-v")
                    .is_none_or(|v| validate_version(v).is_err())
            {
                continue;
            }
            // A row without the uniquely verified full executable contributes
            // no descriptor. It never authorizes another artifact by analogy.
            if let Ok(artifact) = decode_release(None, row) {
                result.push(artifact);
            }
        }
        Ok(result)
    }
}

/// Bounded query plan shared by all candidate paths in one discovery operation.
/// Hint text is only a fixed-origin locator. Re-visiting a held file with a new
/// package hint may add that query even when its bytes were already hashed.
pub(crate) struct LookupPlan {
    cached: Option<String>,
    queried: Vec<String>,
    reserved_bytes: usize,
}
impl LookupPlan {
    pub(crate) fn new(cached: Option<&str>) -> Self {
        Self {
            cached: cached
                .filter(|v| validate_version(v).is_ok())
                .map(str::to_owned),
            queried: Vec::new(),
            reserved_bytes: 0,
        }
    }
    pub(crate) fn candidates(&self, package_hint: Option<&str>) -> Vec<ReleaseQuery> {
        let mut queries = Vec::new();
        for hint in [package_hint, self.cached.as_deref()].into_iter().flatten() {
            if let Ok(query) = ReleaseQuery::from_version_hint(hint) {
                if !queries
                    .iter()
                    .any(|q: &ReleaseQuery| q.url() == query.url())
                {
                    queries.push(query);
                }
            }
        }
        queries.push(ReleaseQuery::latest());
        for page in 1..=INDEX_PAGES {
            queries.push(ReleaseQuery::recent(page).expect("fixed page"));
        }
        queries
    }
    pub(crate) fn reserve(&mut self, query: &ReleaseQuery) -> bool {
        if self.queried.len() == MAX_LOOKUPS || self.queried.iter().any(|url| url == query.url()) {
            return false;
        }
        let Some(reserved) = self.reserved_bytes.checked_add(query.max_metadata_bytes()) else {
            return false;
        };
        if reserved > MAX_DISCOVERY_METADATA_BYTES {
            return false;
        }
        // Charge the complete allowed response before network activity, even
        // if the request later fails. Retained metadata bodies cannot exceed
        // this aggregate bound. The transport may read one additional 16 KiB
        // buffer per refused response to detect an oversized body; that bounded
        // lookahead is not retained or parsed as admitted metadata.
        self.reserved_bytes = reserved;
        self.queried.push(query.url().to_owned());
        true
    }
}

/// Fixed official executable metadata; no caller-selected hash constructor.
/// This proves neither local bytes nor protocol compatibility until the native
/// owner separately verifies both. It is not a download instruction.
#[derive(Debug)]
pub(crate) struct OriginalCliArtifact {
    version: String,
    tag: String,
    release_id: u64,
    asset_id: u64,
    image_bytes: u64,
    image_sha256: [u8; 32],
    asset_url: String,
}

impl OriginalCliArtifact {
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn tag(&self) -> &str {
        &self.tag
    }

    pub(crate) fn release_id(&self) -> u64 {
        self.release_id
    }

    pub(crate) fn asset_id(&self) -> u64 {
        self.asset_id
    }

    pub(crate) fn image_bytes(&self) -> u64 {
        self.image_bytes
    }

    pub(crate) fn image_sha256(&self) -> &[u8; 32] {
        &self.image_sha256
    }

    pub(crate) fn matches_image(&self, bytes: u64, digest: &[u8; 32]) -> bool {
        self.image_bytes == bytes && &self.image_sha256 == digest
    }

    pub(crate) fn asset_url(&self) -> &str {
        &self.asset_url
    }
}

// serde rejects duplicate known fields. Unknown additive metadata is ignored,
// not interpreted as authority or an alternate download source.
#[derive(Deserialize)]
struct ReleaseMetadata {
    url: String,
    assets_url: String,
    html_url: String,
    id: u64,
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<AssetMetadata>,
}

#[derive(Deserialize)]
struct AssetMetadata {
    url: String,
    id: u64,
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

fn decode_metadata(query: ReleaseQuery, body: &[u8]) -> Result<OriginalCliArtifact, Fault> {
    let release: ReleaseMetadata = serde_json::from_slice(body).map_err(|_| Fault::Metadata)?;
    decode_release(query.version.as_deref(), release)
}

fn decode_release(
    expected_version: Option<&str>,
    release: ReleaseMetadata,
) -> Result<OriginalCliArtifact, Fault> {
    let version = release
        .tag_name
        .strip_prefix("rust-v")
        .ok_or(Fault::Release)?;
    validate_version(version).map_err(|_| Fault::Release)?;
    if release.id == 0
        || release.draft
        || release.prerelease
        || expected_version.is_some_and(|hint| hint != version)
        || release.url != format!("{API_ROOT}/{}", release.id)
        || release.assets_url != format!("{API_ROOT}/{}/assets", release.id)
        || release.html_url != format!("{RELEASE_ROOT}/tag/{}", release.tag_name)
    {
        return Err(Fault::Release);
    }
    if release.assets.is_empty() || release.assets.len() > MAX_ASSETS {
        return Err(Fault::Bounds);
    }
    let mut candidates = release
        .assets
        .into_iter()
        .filter(|asset| asset.name == CLI_NAME);
    let asset = candidates.next().ok_or(Fault::Asset)?;
    if candidates.next().is_some() {
        return Err(Fault::Asset);
    }
    let asset_url = format!("{RELEASE_ROOT}/download/{}/{CLI_NAME}", release.tag_name);
    if asset.id == 0
        || asset.state != "uploaded"
        || asset.size == 0
        || asset.size > MAX_IMAGE_BYTES
        || asset.url != format!("{API_ROOT}/assets/{}", asset.id)
        || asset.browser_download_url != asset_url
    {
        return Err(Fault::Asset);
    }
    let image_sha256 = decode_digest(asset.digest.as_deref().ok_or(Fault::Digest)?)?;
    Ok(OriginalCliArtifact {
        version: version.to_owned(),
        tag: release.tag_name,
        release_id: release.id,
        asset_id: asset.id,
        image_bytes: asset.size,
        image_sha256,
        asset_url,
    })
}

fn validate_version(version: &str) -> Result<(), Fault> {
    if version.len() > 32 {
        return Err(Fault::VersionHint);
    }
    let mut parts = version.split('.');
    for _ in 0..3 {
        let part = parts.next().ok_or(Fault::VersionHint)?;
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|b| b.is_ascii_digit())
            || part.parse::<u32>().is_err()
        {
            return Err(Fault::VersionHint);
        }
    }
    if parts.next().is_some() {
        return Err(Fault::VersionHint);
    }
    Ok(())
}

fn decode_digest(digest: &str) -> Result<[u8; 32], Fault> {
    let hex = digest.strip_prefix("sha256:").ok_or(Fault::Digest)?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Fault::Digest);
    }
    let mut bytes = [0; 32];
    for (out, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let digit = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        *out = digit(pair[0]) * 16 + digit(pair[1]);
    }
    if bytes == [0; 32] {
        return Err(Fault::Digest);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    // Exact public executable facts observed from official release metadata on
    // 2026-10-26. These are parser fixtures, not a two-version execution gate.
    const VERSIONS: [(&str, u64, u64, u64, &str); 2] = [
        (
            "0.163.0",
            414_286_110,
            645_225_115,
            341_270_320,
            "2692c8c7bb2cf8b0aefe638741fc90236f4b824c59578ce84994f9de58200993",
        ),
        (
            "0.164.0",
            421_457_270,
            667_993_482,
            354_558_768,
            "6f267830b029ee25d9339864aa867cd45de9f8fd4c5ea007d2e63c13a0c66d36",
        ),
    ];

    fn metadata(version: &str, release_id: u64, asset_id: u64, bytes: u64, digest: &str) -> Value {
        json!({
            "url": format!("{API_ROOT}/{release_id}"),
            "assets_url": format!("{API_ROOT}/{release_id}/assets"),
            "html_url": format!("{RELEASE_ROOT}/tag/rust-v{version}"),
            "id": release_id, "tag_name": format!("rust-v{version}"),
            "draft": false, "prerelease": false,
            "assets": [{
                "url": format!("{API_ROOT}/assets/{asset_id}"),
                "id": asset_id, "name": CLI_NAME, "state": "uploaded", "size": bytes,
                "digest": format!("sha256:{digest}"),
                "browser_download_url": format!("{RELEASE_ROOT}/download/rust-v{version}/{CLI_NAME}")
            }]
        })
    }

    fn baseline() -> Value {
        let (version, release, asset, bytes, digest) = VERSIONS[1];
        metadata(version, release, asset, bytes, digest)
    }

    fn from_bytes(query: ReleaseQuery, body: Vec<u8>) -> Result<OriginalCliArtifact, Fault> {
        let now = Instant::now();
        let url = query.url().to_owned();
        OfficialReleaseResponse::from_native_fetch(query, &url, 200, now, now, body)?.decode(now)
    }

    fn parse(value: &Value) -> Result<OriginalCliArtifact, Fault> {
        from_bytes(ReleaseQuery::latest(), serde_json::to_vec(value).unwrap())
    }

    fn index(value: &Value) -> Result<Vec<OriginalCliArtifact>, Fault> {
        let now = Instant::now();
        let query = ReleaseQuery::recent(1).unwrap();
        let url = query.url().to_owned();
        OfficialReleaseResponse::from_native_fetch(
            query,
            &url,
            200,
            now,
            now,
            serde_json::to_vec(value).unwrap(),
        )?
        .decode_index(now)
    }

    #[test]
    fn representative_full_asset_index_fits_separate_cap_and_both_caps_are_enforced() {
        // Exact saved 0.163/0.164 public release records measure562224/569725
        // bytes. Ten full records, unlike ten truncated parser fixtures, exceed
        // the tag cap. Use bounded ignored presentation data to exercise the
        // actual receipt/parser path at that representative aggregate size.
        let rows = (1..=10)
            .map(|n| {
                let mut row = metadata("0.165.0", n, n + 100, 1, &"55".repeat(32));
                row["body"] = json!("x".repeat(570_000));
                row
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&rows).unwrap();
        assert!(bytes.len() > MAX_METADATA_BYTES && bytes.len() < MAX_INDEX_METADATA_BYTES);
        let now = Instant::now();
        let query = ReleaseQuery::recent(1).unwrap();
        let url = query.url().to_owned();
        assert_eq!(
            OfficialReleaseResponse::from_native_fetch(query, &url, 200, now, now, bytes)
                .unwrap()
                .decode_index(now)
                .unwrap()
                .len(),
            10
        );
        for query in [ReleaseQuery::latest(), ReleaseQuery::recent(1).unwrap()] {
            let url = query.url().to_owned();
            let oversized = vec![b' '; query.max_metadata_bytes() + 1];
            assert!(matches!(
                OfficialReleaseResponse::from_native_fetch(query, &url, 200, now, now, oversized),
                Err(Fault::Bounds)
            ));
        }
        let mut plan = LookupPlan::new(Some("0.150.0"));
        for q in plan.candidates(Some("0.145.0")) {
            assert!(plan.reserve(&q));
        }
        assert!(plan.reserve(&ReleaseQuery::from_version_hint("0.144.0").unwrap()));
        assert_eq!(plan.queried.len(), 6);
        assert_eq!(plan.reserved_bytes, MAX_DISCOVERY_METADATA_BYTES);
        assert!(!plan.reserve(&ReleaseQuery::from_version_hint("0.143.0").unwrap()));
        assert_eq!(plan.reserved_bytes, MAX_DISCOVERY_METADATA_BYTES);
    }

    #[test]
    fn index_finds_installed_one_behind_latest_amid_prereleases_without_hash_lookup() {
        let latest = metadata("0.166.0", 166, 1166, 166_000, &"66".repeat(32));
        let installed = metadata("0.165.0", 165, 1165, 165_000, &"55".repeat(32));
        let mut pre = metadata("0.167.0", 167, 1167, 167_000, &"77".repeat(32));
        pre["prerelease"] = json!(true);
        pre["tag_name"] = json!("rust-v0.167.0-alpha.1");
        let descriptors = index(&json!([pre, latest, installed])).unwrap();
        assert_eq!(descriptors.len(), 2);
        let matched = descriptors
            .iter()
            .find(|a| a.matches_image(165_000, &[0x55; 32]))
            .unwrap();
        assert_eq!(matched.version(), "0.165.0");
        assert!(!matched.matches_image(165_001, &[0x55; 32]));
        assert!(!matched.matches_image(165_000, &[0x66; 32]));
        assert_eq!(
            ReleaseQuery::recent(2).unwrap().url(),
            format!("{API_ROOT}?per_page=10&page=2")
        );
        assert!(ReleaseQuery::recent(0).is_err());
        assert!(ReleaseQuery::recent(3).is_err());
    }

    #[test]
    fn wrong_cached_tag_and_package_hint_never_override_observed_image_digest() {
        let wrong = metadata("0.190.0", 190, 1190, 190_000, &"90".repeat(32));
        let installed = metadata("0.165.0", 165, 1165, 165_000, &"55".repeat(32));
        let descriptor = from_bytes(
            ReleaseQuery::from_version_hint("0.190.0").unwrap(),
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert!(!descriptor.matches_image(165_000, &[0x55; 32]));
        let mut plan = LookupPlan::new(Some("0.190.0"));
        let queries = plan
            .candidates(Some("0.190.0"))
            .into_iter()
            .filter(|q| plan.reserve(q))
            .collect::<Vec<_>>();
        assert_eq!(queries.len(), 4); // duplicated untrusted hint is only one lookup
        let descriptors = index(&json!([wrong, installed])).unwrap();
        let found = descriptors
            .iter()
            .find(|a| a.matches_image(165_000, &[0x55; 32]))
            .unwrap();
        assert_eq!(found.version(), "0.165.0");
        assert!(descriptors
            .iter()
            .all(|a| !a.matches_image(165_000, &[0; 32])));
    }

    #[test]
    fn index_rejects_unbounded_duplicate_rows_and_wrong_response_shape() {
        let rows = (1..=11)
            .map(|n| metadata("0.165.0", n, n + 100, 1, &"55".repeat(32)))
            .collect::<Vec<_>>();
        assert!(matches!(index(&json!(rows)), Err(Fault::Bounds)));
        assert!(matches!(
            index(&json!([baseline(), baseline()])),
            Err(Fault::Release)
        ));
        assert!(matches!(index(&baseline()), Err(Fault::Metadata)));
        assert!(matches!(parse(&json!([baseline()])), Err(Fault::Metadata)));
        let mut wrong = baseline();
        wrong["assets"][0]["browser_download_url"] = json!("https://elsewhere.invalid/codex.exe");
        assert!(index(&json!([wrong])).unwrap().is_empty());
        let now = Instant::now();
        let query = ReleaseQuery::recent(1).unwrap();
        let url = query.url().to_owned();
        let bytes = serde_json::to_vec(&json!([baseline()])).unwrap();
        let receipt =
            OfficialReleaseResponse::from_native_fetch(query, &url, 200, now, now, bytes).unwrap();
        assert!(matches!(
            receipt.decode_index(now + RESPONSE_MAX_AGE + Duration::from_nanos(1)),
            Err(Fault::Time)
        ));
    }

    #[test]
    fn same_held_image_later_package_hint_is_not_lost_and_all_lookups_are_bounded() {
        let mut plan = LookupPlan::new(Some("0.150.0"));
        let first = plan
            .candidates(None)
            .into_iter()
            .filter(|q| plan.reserve(q))
            .map(|q| q.url().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(first.len(), 4); // cached tag, latest, exactly two index pages
        assert!(first[0].ends_with("/tags/rust-v0.150.0"));
        // The native detector retains the same held hash on a first miss. A
        // later package hint can issue its previously unseen fixed-origin tag.
        let later = plan
            .candidates(Some("0.145.0"))
            .into_iter()
            .filter(|q| plan.reserve(q))
            .map(|q| q.url().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(later, vec![format!("{API_ROOT}/tags/rust-v0.145.0")]);
        assert_eq!(
            plan.candidates(Some("0.144.0"))
                .into_iter()
                .filter(|q| plan.reserve(q))
                .count(),
            1
        );
        assert_eq!(
            plan.candidates(Some("0.143.0"))
                .into_iter()
                .filter(|q| plan.reserve(q))
                .count(),
            0
        );
        let mut invalid = LookupPlan::new(Some("../../auth"));
        assert_eq!(
            invalid
                .candidates(None)
                .into_iter()
                .filter(|q| invalid.reserve(q))
                .count(),
            3
        );
    }

    #[test]
    fn adjacent_official_cli_descriptors_and_version_hint_queries() {
        for (version, release, asset, bytes, digest) in VERSIONS {
            let value = metadata(version, release, asset, bytes, digest);
            let query = ReleaseQuery::from_version_hint(version).unwrap();
            assert_eq!(query.url(), format!("{API_ROOT}/tags/rust-v{version}"));
            let result = from_bytes(query, serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(result.version(), version);
            assert_eq!(result.tag(), format!("rust-v{version}"));
            assert_eq!(result.release_id(), release);
            assert_eq!(result.asset_id(), asset);
            assert_eq!(result.image_bytes(), bytes);
            assert_eq!(
                result.image_sha256(),
                &decode_digest(&format!("sha256:{digest}")).unwrap()
            );
            assert_eq!(
                result.asset_url(),
                format!("{RELEASE_ROOT}/download/rust-v{version}/{CLI_NAME}")
            );
        }
    }

    #[test]
    fn future_stable_metadata_and_irrelevant_additions_are_not_version_locked() {
        let mut value = metadata("1.200.9", 1, 2, 400_000_000, VERSIONS[1].4);
        value["new_metadata"] = json!({"future": [true, "ignored"]});
        value["assets"][0]["extra_digest"] = json!("not an authority source");
        let mut unrelated = value["assets"][0].clone();
        unrelated["name"] = json!("codex-app-server-x86_64-pc-windows-msvc.exe");
        unrelated["digest"] = Value::Null;
        value["assets"].as_array_mut().unwrap().push(unrelated);
        assert_eq!(parse(&value).unwrap().version(), "1.200.9");
        assert_eq!(ReleaseQuery::latest().url(), format!("{API_ROOT}/latest"));
    }

    #[test]
    fn query_rejects_noncanonical_or_path_bearing_versions() {
        for version in [
            "",
            "0.164",
            "0.164.0.1",
            "v0.164.0",
            "rust-v0.164.0",
            "0.164.0-alpha.1",
            "0.164.0+build",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "-1.2.3",
            "1.2.3\n",
            "1.2.3/..",
            "1.2.%33",
            "１.2.3",
            "1.2.4294967296",
        ] {
            assert!(
                matches!(
                    ReleaseQuery::from_version_hint(version),
                    Err(Fault::VersionHint)
                ),
                "{version:?}"
            );
        }
    }

    #[test]
    fn native_response_origin_status_and_time_are_bound() {
        let now = Instant::now();
        let bytes = serde_json::to_vec(&baseline()).unwrap();
        for url in [
            "http://api.github.com/repos/openai/codex/releases/latest",
            "https://api.github.com.evil.test/repos/openai/codex/releases/latest",
            "https://api.github.com/repos/other/codex/releases/latest",
            "https://api.github.com/repos/openai/codex/releases/latest?x=1",
            "https://api.github.com/repos/openai/codex/releases/latest/",
            "https://api.github.com/repos/openai/codex/releases/%6catest",
        ] {
            assert!(matches!(
                OfficialReleaseResponse::from_native_fetch(
                    ReleaseQuery::latest(),
                    url,
                    200,
                    now,
                    now,
                    bytes.clone()
                ),
                Err(Fault::Origin)
            ));
        }
        let url = ReleaseQuery::latest().url().to_owned();
        for status in [0, 201, 301, 302, 304, 401, 403, 404, 429, 500] {
            assert!(matches!(
                OfficialReleaseResponse::from_native_fetch(
                    ReleaseQuery::latest(),
                    &url,
                    status,
                    now,
                    now,
                    bytes.clone()
                ),
                Err(Fault::HttpStatus)
            ));
        }
        assert!(matches!(
            OfficialReleaseResponse::from_native_fetch(
                ReleaseQuery::latest(),
                &url,
                200,
                now + Duration::from_secs(1),
                now,
                bytes.clone()
            ),
            Err(Fault::Time)
        ));
        assert!(matches!(
            OfficialReleaseResponse::from_native_fetch(
                ReleaseQuery::latest(),
                &url,
                200,
                now,
                now + FETCH_TIMEOUT + Duration::from_nanos(1),
                bytes.clone()
            ),
            Err(Fault::Time)
        ));
        let response = OfficialReleaseResponse::from_native_fetch(
            ReleaseQuery::latest(),
            &url,
            200,
            now,
            now,
            bytes.clone(),
        )
        .unwrap();
        assert!(matches!(
            response.decode(now + RESPONSE_MAX_AGE + Duration::from_nanos(1)),
            Err(Fault::Time)
        ));
        let response = OfficialReleaseResponse::from_native_fetch(
            ReleaseQuery::latest(),
            &url,
            200,
            now + Duration::from_secs(1),
            now + Duration::from_secs(1),
            bytes,
        )
        .unwrap();
        assert!(matches!(response.decode(now), Err(Fault::Time)));
    }

    #[test]
    fn body_and_asset_counts_are_bounded() {
        assert!(matches!(
            from_bytes(ReleaseQuery::latest(), vec![]),
            Err(Fault::Bounds)
        ));
        assert!(matches!(
            from_bytes(ReleaseQuery::latest(), vec![b' '; MAX_METADATA_BYTES + 1]),
            Err(Fault::Bounds)
        ));
        let mut value = baseline();
        value["assets"] = json!([]);
        assert!(matches!(parse(&value), Err(Fault::Bounds)));
        let asset = baseline()["assets"][0].clone();
        value["assets"] = Value::Array(vec![asset; MAX_ASSETS + 1]);
        assert!(matches!(parse(&value), Err(Fault::Bounds)));
    }

    #[test]
    fn release_identity_and_stability_are_not_advisory() {
        for (field, replacement) in [
            ("id", json!(0)),
            ("draft", json!(true)),
            ("prerelease", json!(true)),
            (
                "url",
                json!("https://api.github.com/repos/other/codex/releases/421457270"),
            ),
            (
                "assets_url",
                json!("https://api.github.com/repos/openai/codex/releases/1/assets"),
            ),
            (
                "html_url",
                json!("https://github.com/openai/codex/releases/tag/rust-v0.163.0"),
            ),
            ("tag_name", json!("rust-v0.164.0-alpha.1")),
        ] {
            let mut value = baseline();
            value[field] = replacement;
            assert!(matches!(parse(&value), Err(Fault::Release)), "{field}");
        }
        assert!(matches!(
            from_bytes(
                ReleaseQuery::from_version_hint("0.163.0").unwrap(),
                serde_json::to_vec(&baseline()).unwrap()
            ),
            Err(Fault::Release)
        ));
    }

    #[test]
    fn selected_asset_is_unique_exact_cli_and_has_canonical_origin() {
        for name in [
            "codex-app-server-x86_64-pc-windows-msvc.exe",
            "codex-x86_64-pc-windows-msvc-bazel.exe",
            "codex-aarch64-pc-windows-msvc.exe",
            "codex-x86_64-pc-windows-msvc.exe.zip",
            "../codex-x86_64-pc-windows-msvc.exe",
            "CODEX-x86_64-pc-windows-msvc.exe",
        ] {
            let mut value = baseline();
            value["assets"][0]["name"] = json!(name);
            assert!(matches!(parse(&value), Err(Fault::Asset)), "{name}");
        }
        for (field, replacement) in [
            ("id", json!(0)), ("state", json!("new")), ("size", json!(0)), ("size", json!(MAX_IMAGE_BYTES + 1)),
            ("url", json!("https://api.github.com/repos/other/codex/releases/assets/667993482")),
            ("browser_download_url", json!("https://github.com/openai/codex/releases/download/rust-v0.164.0/../codex-x86_64-pc-windows-msvc.exe")),
            ("browser_download_url", json!("https://github.com/openai/codex/releases/download/rust-v0.164.0/%63odex-x86_64-pc-windows-msvc.exe")),
            ("browser_download_url", json!("https://github.com/openai/codex/releases/download/rust-v0.164.0/codex-x86_64-pc-windows-msvc.exe?download=1")),
            ("browser_download_url", json!("https://github.com.evil.test/openai/codex/releases/download/rust-v0.164.0/codex-x86_64-pc-windows-msvc.exe")),
        ] {
            let mut value = baseline();
            value["assets"][0][field] = replacement;
            assert!(matches!(parse(&value), Err(Fault::Asset)), "{field}");
        }
        let mut value = baseline();
        value["assets"]
            .as_array_mut()
            .unwrap()
            .push(baseline()["assets"][0].clone());
        assert!(matches!(parse(&value), Err(Fault::Asset)));
    }

    #[test]
    fn digest_is_exact_executable_sha256_not_optional_or_alternate() {
        for replacement in [
            Value::Null,
            json!(""),
            json!("sha512:00"),
            json!(format!("sha256:{}", "0".repeat(64))),
            json!(format!("sha256:{}", "A".repeat(64))),
            json!(format!("sha256:{}", "g".repeat(64))),
            json!(format!("sha256:{}\n", VERSIONS[1].4)),
            json!(format!("sha256:{}", "a".repeat(63))),
        ] {
            let mut value = baseline();
            value["assets"][0]["digest"] = replacement;
            assert!(matches!(parse(&value), Err(Fault::Digest)));
        }
        let mut value = baseline();
        value["assets"][0].as_object_mut().unwrap().remove("digest");
        assert!(matches!(parse(&value), Err(Fault::Digest)));
    }

    #[test]
    fn duplicate_known_keys_and_malformed_shapes_fail_closed() {
        let good = serde_json::to_string(&baseline()).unwrap();
        for duplicate in [
            "\"tag_name\":\"rust-v0.164.0\",",
            "\"assets\":[],",
            "\"id\":421457270,",
        ] {
            let bytes = format!("{{{duplicate}{}", &good[1..]).into_bytes();
            assert!(matches!(
                from_bytes(ReleaseQuery::latest(), bytes),
                Err(Fault::Metadata)
            ));
        }
        let duplicate_digest = good.replace("\"digest\":", "\"digest\":null,\"digest\":");
        assert!(matches!(
            from_bytes(ReleaseQuery::latest(), duplicate_digest.into_bytes()),
            Err(Fault::Metadata)
        ));
        for bytes in [
            b"null".to_vec(),
            b"[]".to_vec(),
            b"{}".to_vec(),
            vec![0xff],
            format!("{good} true").into_bytes(),
        ] {
            assert!(matches!(
                from_bytes(ReleaseQuery::latest(), bytes),
                Err(Fault::Metadata)
            ));
        }
        for field in [
            "url",
            "assets_url",
            "html_url",
            "id",
            "tag_name",
            "draft",
            "prerelease",
            "assets",
        ] {
            let mut value = baseline();
            value.as_object_mut().unwrap().remove(field);
            assert!(matches!(parse(&value), Err(Fault::Metadata)), "{field}");
        }
    }
}

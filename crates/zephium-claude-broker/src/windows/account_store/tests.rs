//! Opt-in native fixtures only. Compiling these is not Windows qualification.
//! Every root is freshly owned and deliberately retained: this increment has no
//! retirement API. The unresolved-guard case retains handles until process exit.
use super::*;

fn fixture() -> (FixtureStoreOwner, StoreLease) {
    let suffix = native::fresh_name().unwrap();
    let suffix = suffix.strip_prefix("Zephium.ClaudeStoreFixture.").unwrap();
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}")).unwrap();
    FixtureStoreOwner::create(profile, key()).unwrap()
}
fn key() -> FixtureKey {
    FixtureKey {
        domain: FixtureDomain::TrialModel,
        profile: 7,
        incarnation: 11,
    }
}
fn settled_by_synthetic_test() -> WorkerSettled {
    WorkerSettled { _private: () }
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn clean_exact_in_process_reopen_preserves_identity_and_profile_sid() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    let identity = receipt.0.config;
    let sids = receipt.0.profile.fixture_sids().unwrap();
    assert!(matches!(
        StoreLease::reopen(&receipt, key()),
        Err(Error::Busy)
    ));
    drop(lease);
    let reopened = StoreLease::reopen(&receipt, key()).unwrap();
    assert!(identity == reopened.inner.config.identity);
    assert!(sids == reopened.inner.receipt.0.profile.fixture_sids().unwrap());
    reopened.verify().unwrap();
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn fresh_collision_never_adopts_reseals_or_changes_existing_state() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    let chain = &lease.inner.chain;
    let parent = &chain[chain.len() - 2].file;
    let expected = &receipt.0;
    assert!(matches!(
        native::create_directory(
            parent,
            &expected.root_name,
            &Policy::directory(&expected.owner_sid, &expected.package_sid, false)
        ),
        Err(Error::Collision)
    ));
    lease.verify().unwrap();
    lease.inner.require_clean().unwrap();
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn lifetime_lease_and_active_guard_outlive_fixture_owner_and_view() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    let active = lease.begin_operation().unwrap();
    assert!(matches!(lease.begin_operation(), Err(Error::Busy)));
    assert!(matches!(lease.inner.require_clean(), Err(Error::Dirty)));
    drop(owner);
    drop(lease);
    assert!(matches!(
        StoreLease::reopen(&receipt, key()),
        Err(Error::Busy)
    ));
    active.config_path().unwrap();
    active.settle(settled_by_synthetic_test()).unwrap();
    StoreLease::reopen(&receipt, key())
        .unwrap()
        .verify()
        .unwrap();
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn dirty_record_refuses_reopen_after_simulated_handle_release() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    // This is state/lock composition, NOT a native crash/job recovery test.
    // No child was created. Simulate only persisted dirty state + OS lock loss.
    lease.inner.write_state(DIRTY).unwrap();
    drop(lease);
    assert!(matches!(
        StoreLease::reopen(&receipt, key()),
        Err(Error::Dirty)
    ));
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn unresolved_guard_drop_keeps_native_lease_and_never_clears_dirty() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    let active = lease.begin_operation().unwrap();
    drop(active); // Deliberate custody retention until this test process exits.
    assert!(matches!(lease.verify(), Err(Error::Quarantined)));
    drop(owner);
    drop(lease);
    assert!(matches!(
        StoreLease::reopen(&receipt, key()),
        Err(Error::Busy)
    ));
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn wrong_profile_or_fixture_domain_never_reopens() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    drop(lease);
    let mut wrong = key();
    wrong.domain = FixtureDomain::ProductModel;
    assert!(matches!(
        StoreLease::reopen(&receipt, wrong),
        Err(Error::Invalid)
    ));
    wrong = key();
    wrong.profile += 1;
    assert!(matches!(
        StoreLease::reopen(&receipt, wrong),
        Err(Error::Invalid)
    ));
    wrong = key();
    wrong.incarnation += 1;
    assert!(matches!(
        StoreLease::reopen(&receipt, wrong),
        Err(Error::Invalid)
    ));
    StoreLease::reopen(&receipt, key()).unwrap();
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn malformed_oversized_and_truncated_state_never_default_to_clean() {
    let cases: &[&[u8]] = &[
        b"",                 // Entirely missing record.
        b"ZCLST001",         // Missing state byte.
        b"ZCLST001\x02",     // Unknown state, correct length.
        b"XCLST001\x00",     // Unknown schema, correct length.
        b"ZCLST001\x00tail", // Clean prefix must not admit an oversized record.
    ];
    for malformed in cases {
        let (owner, lease) = fixture();
        let receipt = owner.receipt();
        {
            let mut marker = lease.inner.state.lock().unwrap();
            marker.set_len(0).unwrap();
            marker.seek(SeekFrom::Start(0)).unwrap();
            marker.write_all(malformed).unwrap();
            marker.sync_all().unwrap();
            marker.seek(SeekFrom::Start(0)).unwrap();
            let mut observed = Vec::new();
            marker.read_to_end(&mut observed).unwrap();
            assert_eq!(observed.as_slice(), *malformed);
        }
        assert!(matches!(lease.begin_operation(), Err(Error::Dirty)));
        drop(lease);
        assert!(matches!(
            StoreLease::reopen(&receipt, key()),
            Err(Error::Dirty)
        ));
    }
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn changed_fresh_marker_acl_is_rejected_without_repair() {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::{
        core::{BOOL, PCWSTR},
        Win32::{
            Foundation::{LocalFree, HANDLE, HLOCAL},
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                    SetSecurityInfo, SE_FILE_OBJECT,
                },
                GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            },
        },
    };
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    let exact = &receipt.0;
    // Only our original held, empty synthetic store's marker is targeted.
    // Narrow its ACL, rather than granting a new principal access to anything.
    let file = File::options()
        .access_mode(0x0006_0080)
        .share_mode(3)
        .custom_flags(0x0020_0000)
        .open(exact.parent.join(&exact.root_name).join(STATE))
        .unwrap();
    assert!(native::identity(&file, false).unwrap() == exact.state);
    let text = super::super::wide(std::ffi::OsStr::new(&format!(
        "O:{}D:P(A;;FA;;;{})",
        exact.owner_sid, exact.owner_sid
    )))
    .unwrap();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(text.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
    }
    .unwrap();
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut dacl = std::ptr::null_mut();
    unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
        .unwrap();
    assert!(present.as_bool() && !dacl.is_null());
    let changed = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl),
            None,
        )
    }
    .ok();
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    // Setup refusal is a failure, never counted as rejection evidence.
    changed.unwrap();
    let mut actual_dacl = std::ptr::null_mut();
    let mut actual = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut actual_dacl),
            None,
            Some(&mut actual),
        )
    }
    .ok()
    .unwrap();
    assert!(!actual_dacl.is_null());
    let ace_count = unsafe { (*actual_dacl).AceCount };
    unsafe { LocalFree(Some(HLOCAL(actual.0))) };
    assert_eq!(ace_count, 1); // Positive proof that the fixture mutation took effect.
    assert!(matches!(lease.verify(), Err(Error::Invalid)));
    assert!(matches!(lease.verify(), Err(Error::Quarantined)));
    drop(file);
    drop(lease);
    assert!(matches!(
        StoreLease::reopen(&receipt, key()),
        Err(Error::Invalid)
    ));
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn replacement_fresh_marker_identity_is_rejected_despite_valid_acl_and_bytes() {
    let (owner, lease) = fixture();
    let receipt = owner.receipt();
    drop(lease);
    let exact = &receipt.0;
    let chain = native::pin_parent(&exact.parent, &exact.owner_sid).unwrap();
    let root = native::open_directory(
        &chain.last().unwrap().file,
        &exact.root_name,
        &Policy::directory(&exact.owner_sid, &exact.package_sid, false),
    )
    .unwrap();
    assert!(root.receipt() == *exact.chain.last().unwrap());
    // Preserve the original file under a new fresh-only name. The root and all
    // ancestors remain pinned while this synthetic replacement is constructed.
    let path = exact.parent.join(&exact.root_name);
    let retained = native::fresh_name().unwrap();
    assert!(matches!(std::fs::symlink_metadata(path.join(&retained)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound));
    std::fs::rename(path.join(STATE), path.join(&retained)).unwrap();
    let mut replacement =
        native::create_file(&root.file, STATE, &Policy::broker_file(&exact.owner_sid)).unwrap();
    replacement.write_all(CLEAN).unwrap();
    replacement.sync_all().unwrap();
    native::sync_directory(&root.file).unwrap();
    assert!(native::identity(&replacement, false).unwrap() != exact.state);
    let original = native::open_file(
        &root.file,
        &retained,
        &Policy::broker_file(&exact.owner_sid),
    )
    .unwrap();
    assert!(native::identity(&original, false).unwrap() == exact.state);
    assert!(matches!(
        StoreLease::reopen(&receipt, key()),
        Err(Error::Invalid)
    ));
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn prospective_worker_path_requires_dirty_state_and_sticky_quarantine() {
    for altered in [CLEAN.as_slice(), b"ZCLST001\x02".as_slice(), b"".as_slice()] {
        let (_owner, lease) = fixture();
        let active = lease.begin_operation().unwrap();
        active.config_path().unwrap(); // Positive active/dirty control first.
        {
            let mut marker = lease.inner.state.lock().unwrap();
            marker.set_len(0).unwrap();
            marker.seek(SeekFrom::Start(0)).unwrap();
            marker.write_all(altered).unwrap();
            marker.sync_all().unwrap();
            marker.seek(SeekFrom::Start(0)).unwrap();
            let mut observed = Vec::new();
            marker.read_to_end(&mut observed).unwrap();
            assert_eq!(observed, altered);
        }
        assert!(matches!(active.config_path(), Err(Error::Dirty)));
        // Even restoring exact bytes cannot restore this quarantined capability.
        {
            let mut marker = lease.inner.state.lock().unwrap();
            marker.set_len(0).unwrap();
            marker.seek(SeekFrom::Start(0)).unwrap();
            marker.write_all(DIRTY).unwrap();
            marker.sync_all().unwrap();
        }
        assert!(matches!(active.config_path(), Err(Error::Quarantined)));
        assert!(matches!(lease.begin_operation(), Err(Error::Busy)));
        // No real worker exists and no settlement proof is invented. This exact
        // intentionally unresolved guard retains native custody until process exit.
        drop(active);
    }
}

#[test]
#[ignore = "native synthetic fixture; explicit Windows qualification only"]
fn parent_spelling_rejects_normalized_aliases_but_accepts_one_os_separator() {
    use std::path::Path;
    let canonical = Path::new(r"C:\fixture\temp");
    for accepted in [r"C:\fixture\temp", r"C:\fixture\temp\"] {
        assert!(native::exact_parent_spelling(
            Path::new(accepted),
            canonical
        ));
    }
    for rejected in [
        r"C:/fixture/temp",
        r"C:\fixture\\temp",
        r"C:\fixture\.\temp",
        r"C:\fixture\temp\\",
        r"c:\fixture\temp",
        r"\\?\C:\fixture\temp",
        r"\\server\fixture\temp",
    ] {
        assert!(!native::exact_parent_spelling(
            Path::new(rejected),
            canonical
        ));
    }
}

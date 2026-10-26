//! Opt-in fresh synthetic durable registration fixtures. No existing native
//! trial root is opened; profile/store residue is deliberately retained.
use super::*;

fn synthetic_settlement() -> WorkerSettled {
    WorkerSettled { _private: () }
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn disk_reopen_drops_all_store_receipts_and_profiles_before_reconstruction() {
    let anchor = FixtureAnchor::fresh().unwrap();
    let profile = native::random_id().unwrap();
    let (key, package, record) = {
        let factory = anchor.create_factory().unwrap();
        let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
        let key = lease.profile_key();
        let package = lease.inner.receipt.0.package_sid.clone();
        let record = lease.inner.receipt.0.durable.clone().unwrap();
        let active = lease.begin_operation().unwrap();
        let paths = active.runtime_paths().unwrap();
        std::fs::write(
            paths.config.join("inert-settings.txt"),
            "synthetic saved value",
        )
        .unwrap();
        std::fs::write(paths.home.join("inert-home.txt"), "synthetic home").unwrap();
        std::fs::write(paths.appdata.join("inert-appdata.txt"), "synthetic appdata").unwrap();
        active.settle(synthetic_settlement()).unwrap();
        (key, package, record)
    };
    // No StoreLease, Profile Arc or in-process Receipt survived that scope.
    let factory = anchor.reopen_factory().unwrap();
    let lease = factory.reopen(profile, PolicyVersion::DataV1).unwrap();
    assert!(lease.profile_key() == key);
    assert!(lease.inner.receipt.0.package_sid == package);
    assert!(lease.inner.receipt.0.durable.as_ref() == Some(&record));
    let active = lease.begin_operation().unwrap();
    let paths = active.runtime_paths().unwrap();
    assert_eq!(
        std::fs::read_to_string(paths.config.join("inert-settings.txt")).unwrap(),
        "synthetic saved value"
    );
    assert_eq!(
        std::fs::read_to_string(paths.home.join("inert-home.txt")).unwrap(),
        "synthetic home"
    );
    assert_eq!(
        std::fs::read_to_string(paths.appdata.join("inert-appdata.txt")).unwrap(),
        "synthetic appdata"
    );
    active.settle(synthetic_settlement()).unwrap();
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn collision_and_lifetime_lock_never_adopt_or_repair() {
    let anchor = FixtureAnchor::fresh().unwrap();
    let factory = anchor.create_factory().unwrap();
    let profile = native::random_id().unwrap();
    let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
    assert!(matches!(
        factory.create(profile, PolicyVersion::DataV1),
        Err(Error::Collision)
    ));
    assert!(matches!(
        factory.reopen(profile, PolicyVersion::DataV1),
        Err(Error::Busy)
    ));
    lease.verify().unwrap();
    lease.inner.require_clean().unwrap();
    drop(lease);
    factory
        .reopen(profile, PolicyVersion::DataV1)
        .unwrap()
        .verify()
        .unwrap();
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn persisted_dirty_and_partial_roots_refuse_without_recovery() {
    let anchor = FixtureAnchor::fresh().unwrap();
    let profile = native::random_id().unwrap();
    {
        let factory = anchor.create_factory().unwrap();
        let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
        lease.inner.write_state(DIRTY).unwrap();
    }
    let factory = anchor.reopen_factory().unwrap();
    assert!(matches!(
        factory.reopen(profile, PolicyVersion::DataV1),
        Err(Error::Dirty)
    ));
    let partial = native::random_id().unwrap();
    native::create_directory(
        &factory.chain.last().unwrap().file,
        &root_name(partial),
        &Policy::broker_directory(&factory.owner_sid),
    )
    .unwrap();
    assert!(factory.reopen(partial, PolicyVersion::DataV1).is_err());
    assert!(matches!(
        factory.create(partial, PolicyVersion::DataV1),
        Err(Error::Collision)
    ));
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn record_policy_corruption_and_replacement_identity_refuse() {
    let anchor = FixtureAnchor::fresh().unwrap();
    let factory = anchor.create_factory().unwrap();
    for fault in 0..4 {
        let profile = native::random_id().unwrap();
        let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
        let root_path = lease.inner.receipt.0.parent.join(root_name(profile));
        let record = lease.inner.receipt.0.durable.clone().unwrap();
        drop(lease);
        assert!(factory
            .reopen(profile, PolicyVersion::FileReplaceV1)
            .is_err());
        match fault {
            0 => {
                std::fs::write(root_path.join(REGISTRATION), b"partial").unwrap();
            }
            1 => {
                let mut changed = record.clone();
                changed.key.profile[0] ^= 1;
                std::fs::write(root_path.join(REGISTRATION), changed.encode()).unwrap();
            }
            2 => {
                let mut changed = record.clone();
                changed.package_sid_hash[0] ^= 1;
                std::fs::write(root_path.join(REGISTRATION), changed.encode()).unwrap();
            }
            _ => {
                std::fs::rename(
                    root_path.join(REGISTRATION),
                    root_path.join("retained-record"),
                )
                .unwrap();
                let root = native::open_directory(
                    &factory.chain.last().unwrap().file,
                    &root_name(profile),
                    &Policy::broker_directory(&factory.owner_sid),
                )
                .unwrap();
                let mut file = native::create_file(
                    &root.file,
                    REGISTRATION,
                    &Policy::broker_file(&factory.owner_sid),
                )
                .unwrap();
                file.write_all(&record.encode()).unwrap();
                file.sync_all().unwrap();
            }
        }
        assert!(factory.reopen(profile, PolicyVersion::DataV1).is_err());
    }
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn deleted_owned_os_profile_is_not_recreated_during_reopen() {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Isolation::DeleteAppContainerProfile;
    let anchor = FixtureAnchor::fresh().unwrap();
    let profile = native::random_id().unwrap();
    let key = {
        let factory = anchor.create_factory().unwrap();
        let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
        lease.profile_key()
    };
    // Explicit removal is limited to the exact fresh synthetic identity after
    // all its handles were dropped; there is no product retirement authority.
    let name = crate::registration::principal_name(key);
    let name = crate::windows::wide(std::ffi::OsStr::new(name.as_str())).unwrap();
    unsafe { DeleteAppContainerProfile(PCWSTR(name.as_ptr())) }.unwrap();
    assert!(anchor
        .reopen_factory()
        .unwrap()
        .reopen(profile, PolicyVersion::DataV1)
        .is_err());
}

#[test]
#[ignore = "read-only native KnownFolder metadata; explicit Windows qualification only"]
fn native_local_app_data_pin_is_read_only_and_replays_exactly() {
    // No trial namespace lookup, enumeration or creation. Only current-user
    // KnownFolder metadata and its held ancestor identities/ACLs are inspected.
    let owner = security::owner_sid().unwrap();
    let chain = native::pin_local_app_data(&owner).unwrap();
    let receipts = chain.iter().map(HeldDirectory::receipt).collect::<Vec<_>>();
    native::verify_chain(&chain, &receipts, &owner).unwrap();
    let canonical = native::canonical_parent(&chain.last().unwrap().file).unwrap();
    let reopened = native::pin_parent(&canonical, &owner).unwrap();
    assert!(
        reopened
            .iter()
            .map(HeldDirectory::receipt)
            .collect::<Vec<_>>()
            == receipts
    );
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn native_current_owner_package_readers_pass_and_foreign_readers_refuse() {
    let anchor = FixtureAnchor::fresh().unwrap();
    let factory = anchor.create_factory().unwrap();
    let profile = native::random_id().unwrap();
    let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
    native::assert_native_profile_reader_policy(
        &lease.inner.chain.last().unwrap().file,
        &lease.inner.receipt.0.owner_sid,
        &lease.inner.receipt.0.package_sid,
    );
    lease.verify().unwrap();
}

#[test]
#[ignore = "native synthetic durable registration; explicit Windows qualification only"]
fn changed_runtime_identity_and_registration_acl_are_not_repaired() {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::{
        core::{BOOL, PCWSTR},
        Win32::{
            Foundation::{LocalFree, HANDLE, HLOCAL},
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo,
                    SE_FILE_OBJECT,
                },
                GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            },
        },
    };
    let anchor = FixtureAnchor::fresh().unwrap();
    let factory = anchor.create_factory().unwrap();
    let profile = native::random_id().unwrap();
    let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
    let root = lease.inner.receipt.0.parent.join(root_name(profile));
    let package = lease.inner.receipt.0.package_sid.clone();
    drop(lease);
    std::fs::rename(root.join("home"), root.join("retained-home")).unwrap();
    let held = native::open_directory(
        &factory.chain.last().unwrap().file,
        &root_name(profile),
        &Policy::broker_directory(&factory.owner_sid),
    )
    .unwrap();
    native::create_directory(
        &held.file,
        "home",
        &Policy::directory(&factory.owner_sid, &package, true),
    )
    .unwrap();
    assert!(factory.reopen(profile, PolicyVersion::DataV1).is_err());
    drop(held);
    let profile = native::random_id().unwrap();
    let lease = factory.create(profile, PolicyVersion::DataV1).unwrap();
    let root = lease.inner.receipt.0.parent.join(root_name(profile));
    let expected = lease.inner.receipt.0.durable.clone().unwrap();
    drop(lease);
    let held = native::open_directory(
        &factory.chain.last().unwrap().file,
        &root_name(profile),
        &Policy::broker_directory(&factory.owner_sid),
    )
    .unwrap();
    let file = File::options()
        .access_mode(0x0006_0080)
        .share_mode(3)
        .custom_flags(0x0020_0000)
        .open(root.join(REGISTRATION))
        .unwrap();
    assert!(native::identity(&file, false).unwrap() == expected.node(Node::Registration).into());
    let text = crate::windows::wide(std::ffi::OsStr::new(&format!(
        "O:{}D:P(A;;FA;;;{})",
        factory.owner_sid, factory.owner_sid
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
    changed.unwrap(); // Setup failure is never counted as rejection evidence.
    assert!(matches!(
        native::open_read_file(
            &held.file,
            REGISTRATION,
            &Policy::broker_file(&factory.owner_sid)
        ),
        Err(Error::Invalid)
    ));
    assert!(factory.reopen(profile, PolicyVersion::DataV1).is_err());
}

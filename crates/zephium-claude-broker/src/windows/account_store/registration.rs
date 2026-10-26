//! Private durable trial factory. No renderer path, existing foreign account,
//! production availability bypass or automatic partial-state recovery.
use super::super::security;
use super::*;
use crate::registration::{
    sid_hash, Node, NodeIdentity, PolicyVersion, ProfileKey, Record, NAMESPACE, RECORD_LEN,
};

const REGISTRATION: &str = "registration.v1";
const RUNTIME: [(&str, Node, bool); 4] = [
    ("home", Node::Home, true),
    ("appdata", Node::AppData, true),
    ("empty-work", Node::Work, false),
    ("scratch", Node::Scratch, true),
];

/// Native discovery is fixed to LocalAppData plus one trial-only namespace.
/// Every store subsequently owns independent clones of the held ancestor chain.
pub(in crate::windows) struct TrialStoreFactory {
    chain: Vec<HeldDirectory>,
    receipts: Vec<native::DirectoryReceipt>,
    owner_sid: String,
}

/// Opaque disposable anchor for synthetic restart/Port qualification only.
/// No caller path or real trial namespace is admitted by this constructor.
#[cfg(all(debug_assertions, feature = "native-fixtures"))]
pub(in crate::windows) struct FixtureAnchor {
    chain: Vec<HeldDirectory>,
    receipts: Vec<native::DirectoryReceipt>,
    owner: String,
}
#[cfg(all(debug_assertions, feature = "native-fixtures"))]
impl FixtureAnchor {
    pub(in crate::windows) fn fresh() -> Result<Self, Error> {
        let owner = security::owner_sid().map_err(|_| Error::Invalid)?;
        let mut chain = native::pin_temp_parent(&owner)?;
        let root = native::create_directory(
            &chain.last().ok_or(Error::Invalid)?.file,
            &native::fresh_name()?,
            &Policy::broker_directory(&owner),
        )?;
        native::sync_directory(&chain.last().ok_or(Error::Invalid)?.file)?;
        chain.push(root);
        let receipts = chain.iter().map(HeldDirectory::receipt).collect();
        Ok(Self {
            chain,
            receipts,
            owner,
        })
    }
    pub(in crate::windows) fn new_profile(&self) -> Result<[u8; 16], Error> {
        native::random_id()
    }
    fn factory(&self, fresh: bool) -> Result<TrialStoreFactory, Error> {
        native::verify_chain(&self.chain, &self.receipts, &self.owner)?;
        let path = native::canonical_parent(&self.chain.last().ok_or(Error::Invalid)?.file)?;
        let chain = native::pin_parent(&path, &self.owner)?;
        if chain.iter().map(HeldDirectory::receipt).collect::<Vec<_>>() != self.receipts {
            return Err(Error::Invalid);
        }
        TrialStoreFactory::under_held_anchor(chain, self.owner.clone(), fresh)
    }
    pub(in crate::windows) fn create_factory(&self) -> Result<TrialStoreFactory, Error> {
        self.factory(true)
    }
    pub(in crate::windows) fn reopen_factory(&self) -> Result<TrialStoreFactory, Error> {
        self.factory(false)
    }
}

/// Paths are native-owned output, never caller input. The ActiveStore that
/// yielded them must remain in process custody until exact physical settlement.
pub(in crate::windows) struct RuntimePaths {
    pub(in crate::windows) root: PathBuf,
    pub(in crate::windows) config: PathBuf,
    pub(in crate::windows) home: PathBuf,
    pub(in crate::windows) appdata: PathBuf,
    pub(in crate::windows) work: PathBuf,
    pub(in crate::windows) temporary: PathBuf,
    /// Documented input base from which Windows derives this package's AC path.
    pub(in crate::windows) localappdata: PathBuf,
    pub(in crate::windows) os_data: PathBuf,
    pub(in crate::windows) os_temp: PathBuf,
}

pub(super) struct HeldRuntime {
    directories: Vec<HeldDirectory>,
    os_chain: Vec<HeldDirectory>,
    os_receipts: Vec<native::DirectoryReceipt>,
}

fn payload(policy: PolicyVersion) -> FixturePayload {
    match policy {
        PolicyVersion::DataV1 => FixturePayload::Data,
        PolicyVersion::FileReplaceV1 => FixturePayload::Replace,
    }
}
fn root_name(profile: [u8; 16]) -> String {
    let mut name = String::from("profile-");
    for byte in profile {
        use std::fmt::Write;
        write!(name, "{byte:02x}").expect("String formatting");
    }
    name
}
fn node(file: &File, directory: bool) -> Result<NodeIdentity, Error> {
    native::identity(file, directory).map(Into::into)
}
fn require_node(file: &File, directory: bool, record: &Record, role: Node) -> Result<(), Error> {
    if node(file, directory)? != record.node(role) {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn read_record(mut file: &File) -> Result<[u8; RECORD_LEN], Error> {
    if file.metadata().map_err(|_| Error::Io)?.len() != RECORD_LEN as u64 {
        return Err(Error::Invalid);
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| Error::Io)?;
    let mut bytes = [0; RECORD_LEN];
    file.read_exact(&mut bytes).map_err(|_| Error::Io)?;
    Ok(bytes)
}
fn empty_work(held: &HeldDirectory) -> Result<(), Error> {
    // Only the broker can mutate this directory. Its exact held identity and
    // non-reparse path are checked before and after enumeration; no payload read.
    let before = native::identity(&held.file, true)?;
    let path = native::canonical_parent(&held.file)?;
    if std::fs::read_dir(path)
        .map_err(|_| Error::Io)?
        .next()
        .is_some()
        || native::identity(&held.file, true)? != before
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

impl TrialStoreFactory {
    pub(in crate::windows) fn open_native() -> Result<Self, Error> {
        let owner_sid = security::owner_sid().map_err(|_| Error::Invalid)?;
        let chain = native::pin_local_app_data(&owner_sid)?;
        Self::under_held_anchor(chain, owner_sid, false)
    }
    fn under_held_anchor(
        mut chain: Vec<HeldDirectory>,
        owner_sid: String,
        fresh: bool,
    ) -> Result<Self, Error> {
        let parent = &chain.last().ok_or(Error::Invalid)?.file;
        let policy = Policy::broker_directory(&owner_sid);
        // Only the fixed structural namespace may already exist. Its exact
        // owner/ACL is inspected, never resealed. No account is adopted here.
        let namespace = match native::create_directory(parent, NAMESPACE, &policy) {
            Ok(directory) => {
                native::sync_directory(parent)?;
                directory
            }
            Err(Error::Collision) if !fresh => native::open_directory(parent, NAMESPACE, &policy)?,
            Err(error) => return Err(error),
        };
        chain.push(namespace);
        let receipts = chain.iter().map(HeldDirectory::receipt).collect();
        Ok(Self {
            chain,
            receipts,
            owner_sid,
        })
    }
    fn clone_chain(&self) -> Result<Vec<HeldDirectory>, Error> {
        if security::owner_sid().map_err(|_| Error::Invalid)? != self.owner_sid {
            return Err(Error::Invalid);
        }
        native::verify_chain(&self.chain, &self.receipts, &self.owner_sid)?;
        self.chain.iter().map(HeldDirectory::try_clone).collect()
    }
    /// No external incarnation or receipt is needed. A fresh native random
    /// incarnation is committed only inside this newly claimed profile root.
    pub(in crate::windows) fn create(
        &self,
        profile: [u8; 16],
        policy: PolicyVersion,
    ) -> Result<StoreLease, Error> {
        let mut chain = self.clone_chain()?;
        let name = root_name(profile);
        let namespace = &chain.last().ok_or(Error::Invalid)?.file;
        let root =
            native::create_directory(namespace, &name, &Policy::broker_directory(&self.owner_sid))?;
        native::sync_directory(namespace)?;
        chain.push(root);
        let root = &chain.last().ok_or(Error::Invalid)?.file;
        let broker = Policy::broker_file(&self.owner_sid);
        let lock = native::create_file(root, LOCK, &broker)?;
        native::lock_exclusive(&lock)?;
        let mut state = native::create_file(root, STATE, &broker)?;
        state.write_all(DIRTY).map_err(|_| Error::Io)?;
        state.sync_all().map_err(|_| Error::Io)?;
        native::sync_directory(root)?;
        // The broker-owned DIRTY record is durable before principal or payload
        // construction. A later error never retries/adopts/repairs this root.
        let key = ProfileKey {
            profile,
            incarnation: native::random_id()?,
        };
        let principal = Arc::new(Profile::create_trial(key).map_err(|_| Error::Invalid)?);
        let (owner_sid, package_sid) = principal.sids().map_err(|_| Error::Invalid)?;
        if owner_sid != self.owner_sid {
            return Err(Error::Invalid);
        }
        let payload = payload(policy);
        let config =
            native::create_directory(root, CONFIG, &payload.policy(&owner_sid, &package_sid))?;
        let runtime = HeldRuntime::create(root, &principal, &owner_sid, &package_sid, payload)?;
        let mut registration = native::create_file(root, REGISTRATION, &broker)?;
        let record = make_record(
            key,
            policy,
            &chain,
            &config,
            &lock,
            &state,
            &registration,
            &runtime,
            &owner_sid,
            &package_sid,
        )?;
        registration
            .write_all(&record.encode())
            .map_err(|_| Error::Io)?;
        registration.sync_all().map_err(|_| Error::Io)?;
        native::sync_directory(root)?;
        let lease = build_lease(
            chain,
            config,
            lock,
            state,
            registration,
            runtime,
            principal,
            record,
            owner_sid,
            package_sid,
        )?;
        lease.inner.verify()?;
        // A failed CLEAN write/flush may have changed bytes. Do not claim that
        // literal DIRTY survived ambiguous I/O: retain and quarantine custody.
        if let Err(error) = lease.inner.write_state(CLEAN) {
            std::mem::forget(lease);
            return Err(error);
        }
        Ok(lease)
    }
    /// Existing account opens are separate, noncreating operations. The native
    /// caller supplies only its real ProfileId and independently admitted policy.
    pub(in crate::windows) fn reopen(
        &self,
        profile: [u8; 16],
        policy: PolicyVersion,
    ) -> Result<StoreLease, Error> {
        let mut chain = self.clone_chain()?;
        let root = native::open_directory(
            &chain.last().ok_or(Error::Invalid)?.file,
            &root_name(profile),
            &Policy::broker_directory(&self.owner_sid),
        )?;
        chain.push(root);
        let root = &chain.last().ok_or(Error::Invalid)?.file;
        let broker = Policy::broker_file(&self.owner_sid);
        let lock = native::open_file(root, LOCK, &broker)?;
        if lock.metadata().map_err(|_| Error::Io)?.len() != 0 {
            return Err(Error::Invalid);
        }
        native::lock_exclusive(&lock)?;
        let state = native::open_file(root, STATE, &broker)?;
        require_file_state(&state, CLEAN)?;
        let registration = native::open_file(root, REGISTRATION, &broker)?;
        let record = Record::decode(
            &read_record(&registration)?,
            profile,
            sid_hash(&self.owner_sid),
            policy,
        )
        .map_err(|_| Error::Invalid)?;
        for (file, directory, role) in [
            (&chain[chain.len() - 3].file, true, Node::Anchor),
            (&chain[chain.len() - 2].file, true, Node::Namespace),
            (root, true, Node::Root),
            (&lock, false, Node::Lock),
            (&state, false, Node::State),
            (&registration, false, Node::Registration),
        ] {
            require_node(file, directory, &record, role)?;
        }
        // Derivation is not profile existence proof. Exact OS Data/Temp paths
        // and identities are opened below; missing nodes are never recreated.
        let principal = Arc::new(Profile::reopen_trial(record.key).map_err(|_| Error::Invalid)?);
        let (owner_sid, package_sid) = principal.sids().map_err(|_| Error::Invalid)?;
        if owner_sid != self.owner_sid || sid_hash(&package_sid) != record.package_sid_hash {
            return Err(Error::Invalid);
        }
        let payload = payload(policy);
        let config =
            native::open_directory(root, CONFIG, &payload.policy(&owner_sid, &package_sid))?;
        require_node(&config.file, true, &record, Node::Config)?;
        let runtime =
            HeldRuntime::reopen(root, &principal, &owner_sid, &package_sid, payload, &record)?;
        let lease = build_lease(
            chain,
            config,
            lock,
            state,
            registration,
            runtime,
            principal,
            record,
            owner_sid,
            package_sid,
        )?;
        lease.inner.verify()?;
        lease.inner.require_clean()?;
        Ok(lease)
    }
}

impl HeldRuntime {
    fn create(
        root: &File,
        profile: &Profile,
        owner: &str,
        package: &str,
        payload: FixturePayload,
    ) -> Result<Self, Error> {
        Self::open(root, profile, owner, package, payload, None)
    }
    fn reopen(
        root: &File,
        profile: &Profile,
        owner: &str,
        package: &str,
        payload: FixturePayload,
        record: &Record,
    ) -> Result<Self, Error> {
        Self::open(root, profile, owner, package, payload, Some(record))
    }
    fn open(
        root: &File,
        profile: &Profile,
        owner: &str,
        package: &str,
        payload: FixturePayload,
        record: Option<&Record>,
    ) -> Result<Self, Error> {
        let mut directories = Vec::new();
        for (name, role, writable) in RUNTIME {
            let policy = if writable {
                payload.policy(owner, package)
            } else {
                Policy::directory(owner, package, false)
            };
            let held = if record.is_some() {
                native::open_directory(root, name, &policy)?
            } else {
                native::create_directory(root, name, &policy)?
            };
            if let Some(record) = record {
                require_node(&held.file, true, record, role)?;
            }
            if role == Node::Work {
                empty_work(&held)?;
            }
            directories.push(held);
        }
        let os_chain = native::pin_profile_data(profile, owner, package)?;
        let count = os_chain.len();
        if count < 2 {
            return Err(Error::Invalid);
        }
        if let Some(record) = record {
            require_node(&os_chain[count - 2].file, true, record, Node::OsData)?;
            require_node(&os_chain[count - 1].file, true, record, Node::OsTemp)?;
        }
        let os_receipts = os_chain.iter().map(HeldDirectory::receipt).collect();
        Ok(Self {
            directories,
            os_chain,
            os_receipts,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn make_record(
    key: ProfileKey,
    policy: PolicyVersion,
    chain: &[HeldDirectory],
    config: &HeldDirectory,
    lock: &File,
    state: &File,
    registration: &File,
    runtime: &HeldRuntime,
    owner: &str,
    package: &str,
) -> Result<Record, Error> {
    let count = chain.len();
    let os_count = runtime.os_chain.len();
    let nodes = [
        node(&chain[count - 3].file, true)?,
        node(&chain[count - 2].file, true)?,
        node(&chain[count - 1].file, true)?,
        node(&config.file, true)?,
        node(lock, false)?,
        node(state, false)?,
        node(registration, false)?,
        node(&runtime.directories[0].file, true)?,
        node(&runtime.directories[1].file, true)?,
        node(&runtime.directories[2].file, true)?,
        node(&runtime.directories[3].file, true)?,
        node(&runtime.os_chain[os_count - 2].file, true)?,
        node(&runtime.os_chain[os_count - 1].file, true)?,
    ];
    Record::new(key, policy, sid_hash(owner), sid_hash(package), nodes).map_err(|_| Error::Invalid)
}
#[allow(clippy::too_many_arguments)]
fn build_lease(
    chain: Vec<HeldDirectory>,
    config: HeldDirectory,
    lock: File,
    state: File,
    registration: File,
    runtime: HeldRuntime,
    profile: Arc<Profile>,
    record: Record,
    owner_sid: String,
    package_sid: String,
) -> Result<StoreLease, Error> {
    let parent = native::canonical_parent(&chain[chain.len() - 2].file)?;
    let receipt = InProcessFixtureReceipt(Arc::new(Receipt {
        parent,
        root_name: root_name(record.key.profile),
        key: FixtureKey {
            domain: FixtureDomain::TrialModel,
            profile: u128::from_be_bytes(record.key.profile),
            incarnation: u128::from_be_bytes(record.key.incarnation),
        },
        payload: payload(record.policy),
        chain: chain.iter().map(HeldDirectory::receipt).collect(),
        config: config.identity,
        lock: native::identity(&lock, false)?,
        state: native::identity(&state, false)?,
        profile,
        owner_sid,
        package_sid,
        durable: Some(record),
    }));
    Ok(StoreLease {
        inner: Arc::new(Inner {
            receipt,
            chain,
            config,
            lock,
            state: Mutex::new(state),
            registration: Some(Mutex::new(registration)),
            runtime: Some(runtime),
            active: Mutex::new(false),
            quarantined: AtomicBool::new(false),
        }),
    })
}

pub(super) fn verify_durable(inner: &Inner) -> Result<(), Error> {
    let receipt = &inner.receipt.0;
    let Some(record) = &receipt.durable else {
        return if inner.registration.is_none() && inner.runtime.is_none() {
            Ok(())
        } else {
            Err(Error::Invalid)
        };
    };
    let runtime = inner.runtime.as_ref().ok_or(Error::Invalid)?;
    let registration = inner
        .registration
        .as_ref()
        .ok_or(Error::Invalid)?
        .lock()
        .map_err(|_| Error::Quarantined)?;
    let root = &inner.chain.last().ok_or(Error::Invalid)?.file;
    native::verify_file(
        root,
        REGISTRATION,
        &registration,
        record.node(Node::Registration).into(),
        &Policy::broker_file(&receipt.owner_sid),
    )?;
    if read_record(&registration)? != record.encode() {
        return Err(Error::Invalid);
    }
    for ((name, role, writable), held) in RUNTIME.into_iter().zip(&runtime.directories) {
        let policy = if writable {
            receipt
                .payload
                .policy(&receipt.owner_sid, &receipt.package_sid)
        } else {
            Policy::directory(&receipt.owner_sid, &receipt.package_sid, false)
        };
        native::verify_directory(root, name, held, &policy)?;
        require_node(&held.file, true, record, role)?;
        if role == Node::Work {
            empty_work(held)?;
        }
    }
    if runtime.directories.len() != RUNTIME.len() {
        return Err(Error::Invalid);
    }
    native::verify_chain(&runtime.os_chain, &runtime.os_receipts, &receipt.owner_sid)?;
    let count = runtime.os_chain.len();
    require_node(
        &runtime.os_chain[count - 2].file,
        true,
        record,
        Node::OsData,
    )?;
    require_node(
        &runtime.os_chain[count - 1].file,
        true,
        record,
        Node::OsTemp,
    )?;
    Ok(())
}

pub(super) fn runtime_paths(inner: &Inner) -> Result<RuntimePaths, Error> {
    let runtime = inner.runtime.as_ref().ok_or(Error::Invalid)?;
    let root = native::canonical_parent(&inner.chain.last().ok_or(Error::Invalid)?.file)?;
    let count = runtime.os_chain.len();
    let os_data = native::canonical_parent(&runtime.os_chain[count - 2].file)?;
    let os_temp = native::canonical_parent(&runtime.os_chain[count - 1].file)?;
    let localappdata = inner
        .receipt
        .0
        .profile
        .owned_environment_base(&os_data)
        .map_err(|_| Error::Invalid)?;
    Ok(RuntimePaths {
        config: root.join(CONFIG),
        home: root.join("home"),
        appdata: root.join("appdata"),
        work: root.join("empty-work"),
        temporary: root.join("scratch"),
        localappdata,
        os_data,
        os_temp,
        root,
    })
}

#[cfg(all(test, feature = "native-fixtures"))]
mod tests;

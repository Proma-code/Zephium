//! Debug-only fixed original startup/config/signed-out experiment; no login/turn.
use crate::{pin::ImageKind, protocol, windows::Session, PinnedImage, RuntimePaths};
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

/// Closed qualifier selector. The supplied file is never run before exact kind,
/// length/hash, single-link and held-path admission. No caller-selected argv.
pub fn qualify_original(kind: &str, path: &Path) -> Result<(), &'static str> {
    let kind = match kind {
        "cli" => ImageKind::Cli,
        "standalone" => ImageKind::Standalone,
        _ => return Err("mode"),
    };
    let image = Arc::new(PinnedImage::open(path, kind)?);
    println!("codex-stage=image-verified; code=0");
    let runtime = Arc::new(RuntimePaths::fresh()?);
    let result = run(Arc::clone(&image), Arc::clone(&runtime), true);
    // run returns only after Session Drop/finish physically settles. The final
    // explicit cleanup is checked, not delegated to best-effort TempDir Drop.
    let runtime = Arc::try_unwrap(runtime).map_err(|_| "runtime-custody")?;
    let cleanup = runtime.close();
    result?;
    cleanup?;
    println!("codex-stage=runtime-cleaned; code=0");
    cancellation(Arc::clone(&image))?;
    persistent_profile(image)?;
    Ok(())
}
fn run(
    image: Arc<PinnedImage>,
    runtime: Arc<RuntimePaths>,
    report: bool,
) -> Result<(), &'static str> {
    let home = runtime.home().to_str().ok_or("runtime-encoding")?;
    let cwd = runtime.cwd().to_str().ok_or("runtime-encoding")?;
    let config_path = runtime.home().join("config.toml");
    let config_path = config_path.to_str().ok_or("runtime-encoding")?;
    let mut session = Session::spawn(image, Arc::clone(&runtime)).map_err(|_| "spawn")?;
    if report {
        println!("codex-stage=owned-process; code=0");
    }
    // One absolute RPC budget, not restarted after each reply.
    let deadline = Instant::now() + Duration::from_secs(60);
    let cancel = AtomicBool::new(false);
    let initialized = exchange(&mut session, protocol::initialize(), 1, deadline, &cancel)?;
    protocol::accept_initialize(&initialized, home).map_err(|_| "initialize-shape")?;
    if report {
        println!("codex-stage=initialized; code=0");
    }
    session
        .send_frame(&protocol::initialized(), deadline, &cancel)
        .map_err(|_| "initialized-send")?;
    // The exact original server always queues this after its initialize reply.
    // Receive exactly once before sending any account request; never skip unknown
    // messages or extend the absolute RPC deadline.
    let mut startup = session
        .recv_frame(deadline, &cancel)
        .map_err(|_| "startup-receive")?;
    let accepted = protocol::accept_startup_status(&startup);
    startup.fill(0);
    if let Err(category) = accepted {
        println!("codex-startup-fault={}", category as u8);
        return Err("startup-notification");
    }
    if report {
        println!("codex-stage=remote-disabled; code=0");
    }
    let account = exchange(&mut session, protocol::account(), 2, deadline, &cancel)?;
    protocol::accept_account(&account).map_err(|_| "unexpected-account")?;
    if report {
        println!("codex-stage=signed-out; code=0");
    }
    let config = exchange(&mut session, protocol::config(cwd), 3, deadline, &cancel)?;
    crate::config::validate_config_readback(&config).map_err(|_| "config-values")?;
    crate::config::validate_fixture_layers(&config, config_path).map_err(|_| "config-layers")?;
    if report {
        println!("codex-stage=owned-config; code=0");
    }
    let requirements = exchange(&mut session, protocol::requirements(), 4, deadline, &cancel)?;
    crate::config::validate_fixture_requirements(&requirements)
        .map_err(|_| "managed-requirements")?;
    if report {
        println!("codex-stage=requirements-owned-login-only; code=0");
    }
    let exit = session
        .finish(Instant::now() + Duration::from_secs(30))
        .map_err(|_| "close")?;
    if exit.code != 0 {
        return Err("exit-nonzero");
    }
    if report {
        println!("codex-stage=exit-zero-job-empty; code=0");
    }
    Ok(())
}
fn cancellation(image: Arc<PinnedImage>) -> Result<(), &'static str> {
    let runtime = Arc::new(RuntimePaths::fresh()?);
    let mut session = Session::spawn(image, Arc::clone(&runtime)).map_err(|_| "cancel-spawn")?;
    // No RPC and no bytes are sent in this second original-client process.
    // Cancellation must settle the actual job/writer before returning its fault.
    let result = session.recv_frame(
        Instant::now() + Duration::from_secs(10),
        &AtomicBool::new(true),
    );
    if result != Err(crate::windows::Fault::Cancelled) {
        return Err("cancel-classification");
    }
    if session
        .finish(Instant::now() + Duration::from_secs(10))
        .is_ok()
    {
        return Err("forced-close-accepted");
    }
    Arc::try_unwrap(runtime)
        .map_err(|_| "cancel-custody")?
        .close()?;
    println!("codex-stage=cancelled-reaped-cleaned; code=0");
    Ok(())
}

fn exchange(
    session: &mut Session,
    request: Vec<u8>,
    id: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<serde_json::Value, &'static str> {
    session
        .send_frame(&request, deadline, cancel)
        .map_err(|_| "send")?;
    let mut bytes = session
        .recv_frame(deadline, cancel)
        .map_err(|_| "receive")?;
    let value = protocol::reply(&bytes, id).map_err(|_| "envelope");
    bytes.fill(0);
    value
}

// Fresh synthetic parent only. Never opens a product or installed-user home.
fn persistent_profile(image: Arc<PinnedImage>) -> Result<(), &'static str> {
    use crate::{
        connection::{AccountState, Connection, ConversationKey, Fault},
        profile::{
            native::{random_id, ProfileLease},
            ProfileId,
        },
    };
    // The product path uses the full CLI; standalone retains its separate A proof.
    if image.kind() != ImageKind::Cli {
        return Ok(());
    }
    crate::vault_probe::verify()?;
    println!("codex-stage=nonsecret-store-roundtrip; code=0");
    let parent = tempfile::Builder::new()
        .prefix("Zephium.CodexProfileFixture.")
        .tempdir()
        .map_err(|_| "profile-fixture-parent")?;
    let profile = ProfileId::from_native(random_id().map_err(|_| "profile-fixture-id")?)
        .ok_or("profile-fixture-id")?;
    let mut record = None;
    let associations = crate::connection::fixtures::MemoryAssociations::default();
    for epoch in 1..=2 {
        let lease =
            ProfileLease::fixture(parent.path(), profile).map_err(|_| "profile-fixture-open")?;
        if let Some(expected) = &record {
            if expected != &lease.record {
                return Err("profile-fixture-identity");
            }
        } else {
            record = Some(lease.record.clone());
        }
        let cancel = AtomicBool::new(false);
        let mut connection = Connection::bootstrap(
            profile,
            epoch,
            lease,
            crate::connection::CompatibleImage::prepare(
                Arc::clone(&image),
                Instant::now() + Duration::from_secs(120),
                &cancel,
            )
            .map_err(|_| "owner-compatibility")?,
            Box::new(associations.clone()),
            crate::preparation_budget::PreparationBudget::ordinary(
                Instant::now() + Duration::from_secs(120),
            )
            .into(),
            &cancel,
        )
        .map_err(|_| "owner-bootstrap")?;
        let scope = connection.scope().clone();
        let deadline = Instant::now() + Duration::from_secs(60);
        while connection.state() == AccountState::Checking {
            if Instant::now() >= deadline {
                return Err("owner-status-timeout");
            }
            let events = connection
                .poll(&scope, &cancel)
                .map_err(|_| "owner-status-poll")?;
            if events.into_iter().any(|event| {
                !matches!(
                    event.into_event(),
                    crate::connection::Event::Account(AccountState::SignedOut)
                )
            }) {
                return Err("owner-status-event");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if connection.state() != AccountState::SignedOut || connection.catalog().is_some() {
            return Err("owner-status-identity");
        }
        if connection.refresh_catalog(&scope, &cancel) != Err(Fault::NotConnected) {
            return Err("owner-catalog-gate");
        }
        let key = ConversationKey::from_native("synthetic-unsent".into()).ok_or("owner-key")?;
        if connection.start_text(
            &scope,
            crate::connection::TextRequest {
                admission: crate::connection::TextAdmission::from_native(
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(std::sync::atomic::AtomicU64::new(1)),
                    1,
                ),
                reply: 1,
                previous_reply: None,
                key,
                new_conversation: true,
                model: "synthetic-model".into(),
                effort: crate::catalog::EffortSelection::Explicit(crate::catalog::Effort::High),
                text: "synthetic never sent".into(),
            },
            &cancel,
        ) != Err(Fault::NotConnected)
        {
            return Err("owner-turn-gate");
        }
        connection.close().map_err(|_| "owner-close")?;
        if epoch == 1 {
            println!("codex-stage=persistent-profile-created; code=0");
        } else {
            println!("codex-stage=persistent-profile-reopened; code=0");
        }
    }
    parent.close().map_err(|_| "profile-fixture-cleanup")?;
    println!("codex-stage=typed-owner-preauth-refused; code=0");
    Ok(())
}

/// Exact two original full-CLI releases, fresh managed policy, no account home.
/// This mode performs only initialize/config/feature/read-signed-out/close.
pub fn qualify_compatibility(version: &str, path: &Path) -> Result<(), &'static str> {
    let expected = crate::pin::ReviewedCli::parse(version).ok_or("compatibility-version")?;
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(120);
    let candidate =
        crate::image::CliCandidate::open(path).map_err(|_| "compatibility-image-open")?;
    let hashed = candidate
        .hash(deadline, &cancel)
        .map_err(|_| "compatibility-image-hash")?;
    if hashed.reference() != Some(expected) {
        return Err("compatibility-image-pin");
    }
    let image = Arc::new(
        hashed
            .admit_reference()
            .map_err(|_| "compatibility-image-admission")?,
    );
    if image.version() != version {
        return Err("compatibility-image-version");
    }
    let _prepared = crate::connection::CompatibleImage::prepare(image, deadline, &cancel)
        .map_err(|_| "compatibility-probe")?;
    println!("codex-compatibility-{version}: native-code=0");
    Ok(())
}

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
    let result = run(Arc::clone(&image), Arc::clone(&runtime));
    // run returns only after Session Drop/finish physically settles. The final
    // explicit cleanup is checked, not delegated to best-effort TempDir Drop.
    let runtime = Arc::try_unwrap(runtime).map_err(|_| "runtime-custody")?;
    let cleanup = runtime.close();
    result?;
    cleanup?;
    println!("codex-stage=runtime-cleaned; code=0");
    cancellation(image)?;
    Ok(())
}
fn run(image: Arc<PinnedImage>, runtime: Arc<RuntimePaths>) -> Result<(), &'static str> {
    let home = runtime.home().to_str().ok_or("runtime-encoding")?;
    let cwd = runtime.cwd().to_str().ok_or("runtime-encoding")?;
    let config_path = runtime.home().join("config.toml");
    let config_path = config_path.to_str().ok_or("runtime-encoding")?;
    let mut session = Session::spawn(image, Arc::clone(&runtime)).map_err(|_| "spawn")?;
    println!("codex-stage=owned-process; code=0");
    // One absolute RPC budget, not restarted after each reply.
    let deadline = Instant::now() + Duration::from_secs(60);
    let cancel = AtomicBool::new(false);
    let initialized = exchange(&mut session, protocol::initialize(), 1, deadline, &cancel)?;
    protocol::accept_initialize(&initialized, home).map_err(|_| "initialize-shape")?;
    println!("codex-stage=initialized; code=0");
    session
        .send_frame(&protocol::initialized(), deadline, &cancel)
        .map_err(|_| "initialized-send")?;
    let account = exchange(&mut session, protocol::account(), 2, deadline, &cancel)?;
    protocol::accept_account(&account).map_err(|_| "unexpected-account")?;
    println!("codex-stage=signed-out; code=0");
    let config = exchange(&mut session, protocol::config(cwd), 3, deadline, &cancel)?;
    crate::config::validate_config_readback(&config).map_err(|_| "config-values")?;
    crate::config::validate_fixture_layers(&config, config_path).map_err(|_| "config-layers")?;
    println!("codex-stage=owned-config; code=0");
    let requirements = exchange(&mut session, protocol::requirements(), 4, deadline, &cancel)?;
    crate::config::validate_fixture_requirements(&requirements)
        .map_err(|_| "managed-requirements")?;
    println!("codex-stage=requirements-empty; code=0");
    let exit = session
        .finish(Instant::now() + Duration::from_secs(30))
        .map_err(|_| "close")?;
    if exit.code != 0 {
        return Err("exit-nonzero");
    }
    println!("codex-stage=exit-zero-job-empty; code=0");
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

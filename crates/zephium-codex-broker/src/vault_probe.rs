//! Nonsecret OS-store availability prerequisite, not Codex auth/key persistence.
//! One preflight-absent nonce target; no enumeration, borrowed record or fallback.
use crate::profile::native::random_id;
use zephium_credentials::{self as vault, VaultError};

pub(crate) fn verify() -> Result<(), &'static str> {
    let nonce=random_id().map_err(|_|"vault-nonce")?;
    let suffix:String=nonce.iter().map(|b|format!("{b:02x}")).collect();
    let target=format!("app.zephium.subscription-trial.codex-store-check.{suffix}");
    let _turn=vault::turn().map_err(|_|"vault-busy")?;
    if vault::present(&target).map_err(|_|"vault-presence")? {return Err("vault-collision");}
    let mut content=b"Zephium nonsecret Codex store check v1:".to_vec();content.extend_from_slice(&nonce);
    vault::write(&target,&content).map_err(|_|"vault-write")?;
    let result=vault::read(&target);
    match result {
        Ok(bytes) if bytes.as_slice()==content => {
            vault::delete(&target).map_err(|_|"vault-delete")?;
            match vault::read(&target) {Err(VaultError::Missing)=>Ok(()),_=>Err("vault-delete-proof")}
        }
        // Never delete a different value or use an unverified target as cleanup
        // authority. A failure leaves only our unique nonsecret probe, not auth.
        _=>Err("vault-readback"),
    }
}

//! Interactive first-run setup for `darn`.
//!
//! Handles signer generation when no global config exists.

use std::io::IsTerminal;

use darn_core::{config, signer};
use subduction_core::peer::id::PeerId;

/// Checks if first-run setup is needed and runs it interactively.
///
/// Returns `Ok(true)` if setup was completed (or already existed),
/// `Ok(false)` if the user declined setup.
///
/// # Errors
///
/// Returns an error if signer generation fails.
pub(crate) fn ensure_signer() -> eyre::Result<bool> {
    if config::global_signer_exists() {
        return Ok(true);
    }

    let signer_dir = config::global_signer_dir()?;
    let key_path = signer_dir.join("signing_key.ed25519");

    // Non-interactive mode: auto-generate signer
    if !std::io::stdin().is_terminal() {
        println!("No signer found. Generating Ed25519 keypair...");
        println!("  Location: {}", key_path.display());

        let s = signer::generate_and_save(&signer_dir)?;
        let peer_id: PeerId = s.verifying_key().into();
        let peer_id_str = bs58::encode(peer_id.as_bytes()).into_string();

        println!("  Peer ID: {peer_id_str}");
        return Ok(true);
    }

    // Interactive mode: use cliclack prompts
    cliclack::intro("Welcome to darn! 🪡🧦")?;

    cliclack::log::info(format!(
        "No signer found. darn needs to generate an Ed25519 keypair\n\
             to identify you to peers.\n\n\
             Location: {}",
        key_path.display()
    ))?;
    cliclack::log::remark("Keys are stored locally, never uploaded.")?;

    let proceed: bool = cliclack::confirm("Generate signer now?")
        .initial_value(true)
        .interact()?;

    if !proceed {
        cliclack::outro("Setup cancelled.")?;
        return Ok(false);
    }

    let spinner = cliclack::spinner();
    spinner.start("Generating Ed25519 keypair...");

    let s = signer::generate_and_save(&signer_dir)?;
    let peer_id: PeerId = s.verifying_key().into();
    let peer_id_str = bs58::encode(peer_id.as_bytes()).into_string();

    spinner.stop("Signer generated!");

    cliclack::note("Your Peer ID", &peer_id_str)?;

    cliclack::outro("You can share your Peer ID with collaborators.")?;

    Ok(true)
}

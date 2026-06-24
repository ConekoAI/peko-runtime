//! Issue #14 — manifest signature verification on import.
//!
//! Builds a real `.agent` package, then exercises the unpackager's
//! signature gate with five scenarios:
//!
//!   1. green: signed manifest imports successfully
//!   2. red:   tampered manifest byte fails with signature_verification_failed
//!   3. red:   stripped signature fails (no silent fallback to "unsigned")
//!   4. red:   wrong-key signature fails (signed by author A, claims author B)
//!   5. red:   `--force` does NOT bypass signature (a security check, not a format check)
//!   6. green: `--allow-unsigned-agent` permits an unsigned import
//!
//! These tests run without a daemon or registry — they call the
//! [`peko::registry::packaging::Unpackager`] directly against an in-memory file
//! map, which is exactly the path the CLI's `agent import` command
//! eventually reaches through the daemon IPC.
//!
//! See: <https://github.com/ConekoAI/peko-runtime/issues/14>

use base64::Engine;
use peko::identity::keys::KeyPair;
use peko::identity::{DIDDocument, Identity, VerificationMethod};
use peko::registry::packaging::manifest::AgentManifest;
use peko::registry::packaging::validation::ValidationResult;
use peko::registry::packaging::{ImportOptions, Unpackager};
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

// ── Test fixture builders ───────────────────────────────────────────

/// Build a fresh ed25519 keypair + DID document, return both the
/// keypair (for signing) and the DID document bytes (for the
/// `identity/did.json` file in the package).
fn fresh_identity() -> (KeyPair, DIDDocument, String) {
    let keypair = KeyPair::generate();
    let pk_bytes = keypair.verifying_key.as_bytes();
    let key_hash = blake3::hash(pk_bytes).to_hex().to_string()[..16].to_string();
    let did = format!("did:peko:local:test:{key_hash}");
    let multibase = format!("z{}", bs58::encode(pk_bytes).into_string());
    let doc = DIDDocument {
        context: vec!["https://www.w3.org/ns/did/v1".into()],
        id: did.clone(),
        verification_method: vec![VerificationMethod {
            id: format!("{did}#keys-1"),
            key_type: "Ed25519VerificationKey2020".into(),
            controller: did.clone(),
            public_key_multibase: multibase,
        }],
        authentication: vec![format!("{did}#keys-1")],
        assertion_method: vec![format!("{did}#keys-1")],
        service: vec![],
        created: "2026-06-17T00:00:00Z".into(),
        updated: "2026-06-17T00:00:00Z".into(),
    };
    (keypair, doc, did)
}

/// Build the file contents that go in the package, returned with
/// the data needed to wire them into the manifest (DID string).
fn build_file_contents(
    signer: &KeyPair,
    did_doc: &DIDDocument,
) -> (Vec<u8>, Vec<u8>, Vec<u8>, String) {
    let did_json = serde_json::to_vec_pretty(did_doc).unwrap();

    let config_toml = r#"
name = "sig-test"
description = "Signature test agent"
auto_accept_trusted = false
default_timeout_seconds = 300

[provider]
provider_type = "openai"
api_key = "sk-test"
default_model = "gpt-4o-mini"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "gpt-4o-mini"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0
"#;
    let config_bytes = config_toml.as_bytes().to_vec();

    let key_export = signer.export();
    let keys_bytes = serde_json::to_vec(&key_export).unwrap();

    (did_json, config_bytes, keys_bytes, did_doc.id.clone())
}

/// Compute the SHA-256 checksum the manifest stores, matching the
/// format `AgentManifest::compute_checksum` produces.
fn sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256:{:x}", h.finalize())
}

/// Build a manifest signed by `signer` for the given agent name + DID.
///
/// Mirrors `Packager::sign_manifest`: zero the signature, re-serialize
/// via `to_toml`, sign, base64url-no-pad encode. The resulting bytes
/// are what the unpacker reads as `manifest.toml` — they include the
/// signature because we then set the field on the in-memory manifest
/// and re-serialize (also mirroring the packager).
///
/// Like the packager, this function does NOT include `manifest.toml`
/// in its own checksums map — the manifest.toml bytes are written to
/// the archive *after* signing, and `validate_package` only verifies
/// checksums of files listed in the manifest.
fn build_signed_manifest(
    signer: &KeyPair,
    name: &str,
    did: &str,
    did_json: &[u8],
    config_bytes: &[u8],
    keys_bytes: &[u8],
) -> Vec<u8> {
    let mut manifest = AgentManifest::new(name, "1.0.0", did);
    manifest.agent.description = Some("signature-test-fixture".to_string());

    // Add real checksums for the three on-disk files.
    manifest.add_file("identity/did.json", did_json);
    manifest.add_file("config/agent.toml", config_bytes);
    manifest.add_file("identity/keys.enc", keys_bytes);

    // Reconstruct the signed bytes (signature field zeroed).
    let manifest_for_signing = AgentManifest {
        signatures: peko::registry::packaging::manifest::Signatures {
            manifest: String::new(),
            algorithm: "ed25519".to_string(),
        },
        ..manifest.clone()
    };
    let signed_bytes = manifest_for_signing.to_toml().unwrap().into_bytes();

    // Sign and base64url-encode.
    let signature = signer.sign(&signed_bytes);
    manifest.signatures.manifest =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
    manifest.signatures.algorithm = "ed25519".to_string();

    // Re-serialize with the signature field set (this is the on-disk
    // `manifest.toml` the unpacker will see).
    manifest.to_toml().unwrap().into_bytes()
}

/// Build a fully-populated files map for the unpackager.
fn build_files_map(
    manifest_bytes: &[u8],
    did_json: &[u8],
    config_bytes: &[u8],
    keys_bytes: &[u8],
) -> HashMap<String, Vec<u8>> {
    let mut files = HashMap::new();
    files.insert("manifest.toml".to_string(), manifest_bytes.to_vec());
    files.insert("identity/did.json".to_string(), did_json.to_vec());
    files.insert("config/agent.toml".to_string(), config_bytes.to_vec());
    files.insert("identity/keys.enc".to_string(), keys_bytes.to_vec());
    files
}

fn import_options(allow_unsigned: bool, force: bool) -> ImportOptions {
    ImportOptions {
        new_name: Some("imported-sig-test".to_string()),
        passphrase: None,
        rotate_keys: false,
        import_sessions: false,
        import_workspace: false,
        skip_validation: false,
        force,
        team: None,
        allow_unsigned,
    }
}

async fn run_import(
    files: &HashMap<String, Vec<u8>>,
    opts: ImportOptions,
) -> Result<peko::registry::packaging::ImportResult, anyhow::Error> {
    // Write the files map to a tar.gz, point the Unpackager at it,
    // and import. (import_from_files is pub(crate) and not reachable
    // from external integration tests.)
    let temp = TempDir::new().expect("tempdir");
    let package_path = temp.path().join("test.agent");
    write_tar_gz(&package_path, files).expect("write package");

    let base_dir = TempDir::new().expect("tempdir for base");
    let unpackager = Unpackager::new(&package_path).with_base_dir(base_dir.path());
    unpackager.import(opts).await
}

fn write_tar_gz(path: &Path, files: &HashMap<String, Vec<u8>>) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    for (name, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_path(name)?;
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_slice())?;
    }
    tar.finish()?;
    Ok(())
}

/// Flip one character in the agent name `sig-test` → `sxg-test`.
/// This changes the manifest content without breaking TOML parseability.
fn tamper_agent_name(manifest_bytes: &mut [u8]) {
    let needle = b"name = \"sig-test\"";
    let pos = manifest_bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("agent name line should exist in test manifest");
    // Flip the 'i' in 'sig' (offset 9 from start of `name = "sig-test"`).
    manifest_bytes[pos + 9] ^= 0x01;
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn signed_manifest_imports_successfully() {
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);
    let manifest_bytes = build_signed_manifest(
        &signer,
        "sig-test",
        &did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    let result = run_import(&files, import_options(false, false))
        .await
        .expect("signed manifest should import");
    assert_eq!(result.name, "imported-sig-test");
}

#[tokio::test]
async fn tampered_manifest_byte_fails_signature_check() {
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);
    let mut manifest_bytes = build_signed_manifest(
        &signer,
        "sig-test",
        &did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );
    tamper_agent_name(&mut manifest_bytes);
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    let err = run_import(&files, import_options(false, false))
        .await
        .expect_err("tampered manifest should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("signature_verification_failed"),
        "expected stable error code in error, got: {msg}"
    );
}

#[tokio::test]
async fn stripped_signature_fails_when_not_allowed() {
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);

    // Build a manifest that was never signed (signatures.manifest is
    // empty by default). Add real checksums so validation can pass
    // *if* the signature gate lets it through.
    let mut manifest = AgentManifest::new("sig-test", "1.0.0", &did);
    manifest.agent.description = Some("signature-test-fixture".to_string());
    manifest.add_file("identity/did.json", &did_json);
    manifest.add_file("config/agent.toml", &config_bytes);
    manifest.add_file("identity/keys.enc", &keys_bytes);
    let manifest_bytes = manifest.to_toml().unwrap().into_bytes();
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    let err = run_import(&files, import_options(false, false))
        .await
        .expect_err("unsigned manifest should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("signature_verification_failed"),
        "expected signature_verification_failed, got: {msg}"
    );
}

#[tokio::test]
async fn wrong_key_signature_is_rejected() {
    let (author, _, _) = fresh_identity();
    let (imposter, mut imposter_did_doc, imposter_did) = fresh_identity();
    // Swap the imposter's DID doc to advertise the author's public
    // key — a tampered package would do this to claim a different
    // identity.
    {
        let pk_bytes = author.verifying_key.as_bytes();
        let multibase = format!("z{}", bs58::encode(pk_bytes).into_string());
        imposter_did_doc.verification_method[0].public_key_multibase = multibase;
    }
    let _imposter_identity =
        Identity::from_did_document_and_key(imposter_did_doc.clone(), imposter.export()).unwrap();

    // Manifest is signed by `imposter`. The DID in the manifest and
    // the package DID doc both claim `imposter_did`, but the public
    // key in the DID doc is actually `author`'s. The signature won't
    // verify against the advertised public key.
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&imposter, &imposter_did_doc);
    let manifest_bytes = build_signed_manifest(
        &imposter,
        "sig-test",
        &imposter_did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    let err = run_import(&files, import_options(false, false))
        .await
        .expect_err("wrong-key signature should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("signature_verification_failed"),
        "expected signature_verification_failed, got: {msg}"
    );
}

#[tokio::test]
async fn force_flag_does_not_bypass_signature() {
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);
    let mut manifest_bytes = build_signed_manifest(
        &signer,
        "sig-test",
        &did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );
    tamper_agent_name(&mut manifest_bytes);
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    // force: true must NOT bypass signature verification.
    let err = run_import(&files, import_options(false, true))
        .await
        .expect_err("force should NOT bypass signature failure");
    let msg = err.to_string();
    assert!(
        msg.contains("signature_verification_failed"),
        "force should not skip signature check, got: {msg}"
    );
}

#[tokio::test]
async fn allow_unsigned_permits_unsigned_import() {
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);

    // Build an unsigned manifest (signatures.manifest is empty).
    let mut manifest = AgentManifest::new("sig-test", "1.0.0", &did);
    manifest.agent.description = Some("signature-test-fixture".to_string());
    manifest.add_file("identity/did.json", &did_json);
    manifest.add_file("config/agent.toml", &config_bytes);
    manifest.add_file("identity/keys.enc", &keys_bytes);
    let manifest_bytes = manifest.to_toml().unwrap().into_bytes();
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    // allow_unsigned: true should let the import succeed.
    let result = run_import(&files, import_options(true, false))
        .await
        .expect("allow_unsigned should permit unsigned import");
    assert_eq!(result.name, "imported-sig-test");
    // Sanity: the validation result is still computed; the import
    // itself doesn't depend on it when allow_unsigned is set.
    let _v: ValidationResult = result.validation;
}

#[tokio::test]
async fn full_registry_round_trip_preserves_signed_bytes() {
    // Issue #14: end-to-end test that exercises the actual
    // registry push→pull→export_package→import flow and asserts
    // the bytes the unpackager verifies against are byte-identical
    // to the bytes the packager signed. This is the real CI
    // failure surface (s3_agent_registry_roundtrip).
    use peko::registry::AgentRegistry;

    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);

    // Push the .agent file into a fresh AgentRegistry. This is
    // what `peko agent push --file <file>` does.
    let registry_dir = TempDir::new().expect("registry dir");
    let registry = AgentRegistry::new(registry_dir.path());
    registry.init().await.expect("registry init");

    // Build a layer tarball per layer the manifest references,
    // mirroring `load_agent_file_into_registry` in
    // `src/commands/agent/handlers.rs:772`.
    fn build_layer_tarball(layer_files: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for (path, content) in layer_files {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append(&header, content.as_slice()).unwrap();
            }
            tar.finish().unwrap();
        }
        buf
    }
    use peko::registry::packaging::types::compute_digest;
    let mut config_files = std::collections::BTreeMap::new();
    config_files.insert("agent.toml".to_string(), config_bytes.clone());
    let config_tar = build_layer_tarball(&config_files);
    let config_digest = compute_digest(&config_tar);

    let mut identity_files = std::collections::BTreeMap::new();
    identity_files.insert("did.json".to_string(), did_json.clone());
    identity_files.insert("keys.enc".to_string(), keys_bytes.clone());
    let identity_tar = build_layer_tarball(&identity_files);
    let identity_digest = compute_digest(&identity_tar);

    // Build a manifest whose `packaging.files` lists the actual
    // files (with their checksums) and whose `layers` points at
    // the digests we just computed. This is what
    // `Packager::export` produces.
    let mut manifest = AgentManifest::new("sig-test", "1.0.0", &did);
    manifest.agent.description = Some("signature-test-fixture".to_string());
    manifest.add_file("identity/did.json", &did_json);
    manifest.add_file("config/agent.toml", &config_bytes);
    manifest.add_file("identity/keys.enc", &keys_bytes);
    use peko::registry::packaging::manifest::AgentLayers;
    manifest.layers = Some(AgentLayers {
        config: Some(config_digest.clone()),
        identity: Some(identity_digest.clone()),
        ..Default::default()
    });

    // Sign the manifest.
    let manifest_for_signing = AgentManifest {
        signatures: peko::registry::packaging::manifest::Signatures {
            manifest: String::new(),
            algorithm: "ed25519".to_string(),
        },
        ..manifest.clone()
    };
    let signed_bytes = manifest_for_signing.to_toml().unwrap().into_bytes();
    let signature = signer.sign(&signed_bytes);
    use base64::Engine;
    manifest.signatures.manifest =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
    manifest.signatures.algorithm = "ed25519".to_string();
    let _original_manifest_bytes = manifest.to_toml().unwrap().into_bytes();

    registry
        .store_layer(&config_digest, &config_tar)
        .await
        .expect("store config");
    registry
        .store_layer(&identity_digest, &identity_tar)
        .await
        .expect("store identity");
    registry
        .store_manifest(&manifest, Some("sig-test:v1"))
        .await
        .expect("store manifest");

    // Now export — this is the path `peko agent pull` takes to
    // materialize a .agent file from the registry.
    let export_dir = TempDir::new().expect("export dir");
    let export_path = export_dir.path().join("round-tripped.agent");
    registry
        .export_package("sig-test:v1", &export_path)
        .await
        .expect("export_package");

    // Read the exported manifest.toml and verify.
    let file = std::fs::File::open(&export_path).expect("open");
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut exported_bytes = None;
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").to_string_lossy().to_string();
        if path == "manifest.toml" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf).expect("read");
            exported_bytes = Some(buf);
            break;
        }
    }
    let exported_bytes = exported_bytes.expect("manifest.toml in archive");

    // The unpackager's verifier reconstructs the canonical signed
    // bytes from the exported manifest. Both must match the
    // signed bytes from the original.
    let status = peko::registry::packaging::signature::verify_manifest_signature(
        &exported_bytes,
        &did_json,
        false,
    )
    .expect("exported signature must verify");
    assert_eq!(
        status,
        peko::registry::packaging::signature::SignatureStatus::Verified
    );
}

#[test]
fn manifest_round_trip_produces_identical_bytes() {
    // Issue #14 surfaced a real determinism bug: the packager signs
    // `packaging.files` in insertion order, but
    // `AgentRegistry::export_package` rebuilds the manifest on pull
    // and sorts the file list. If the two sides disagree on the
    // field order, the signed bytes from the original export do
    // not match the re-serialized bytes on import, and signature
    // verification fails spuriously. This test exercises the
    // serde round-trip that both the packager (sign-then-write) and
    // the registry (load-then-rebuild-then-write) perform, and
    // asserts the bytes are byte-identical.
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);
    let manifest_bytes = build_signed_manifest_pinned(
        &signer,
        "sig-test",
        &did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );

    // Round-trip: parse the signed manifest back from TOML, then
    // re-serialize. This is what the registry does on
    // `get_manifest_by_tag` followed by `manifest.to_toml()` in
    // `export_package`.
    let parsed: AgentManifest =
        AgentManifest::from_toml(std::str::from_utf8(&manifest_bytes).expect("manifest utf-8"))
            .expect("parse signed manifest");
    let roundtripped = parsed.to_toml().expect("re-serialize");

    // Strip the manifest.toml trailing newline the TOML writer
    // may or may not emit, then compare. The signature is over
    // the canonical reconstruction (which always has a trailing
    // newline), so we normalize.
    let normalize = |mut b: Vec<u8>| -> Vec<u8> {
        while b.last() == Some(&b'\n') {
            b.pop();
        }
        b.push(b'\n');
        b
    };
    assert_eq!(
        normalize(manifest_bytes.clone()),
        normalize(roundtripped.into_bytes()),
        "signed manifest must round-trip byte-identically through serde"
    );
}

#[test]
fn manifest_signing_is_byte_stable() {
    // Regression guard: if `toml::to_string_pretty` ever stops being
    // deterministic in a way that affects our reconstructed signing
    // bytes, the packager and verifier will silently diverge. This
    // test re-derives the canonical bytes twice (with a pinned
    // `created_at` to avoid timestamp noise) and asserts equality.
    let (signer, did_doc, did) = fresh_identity();
    let (did_json, config_bytes, keys_bytes, _) = build_file_contents(&signer, &did_doc);
    let m1 = build_signed_manifest_pinned(
        &signer,
        "sig-test",
        &did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );
    let m2 = build_signed_manifest_pinned(
        &signer,
        "sig-test",
        &did,
        &did_json,
        &config_bytes,
        &keys_bytes,
    );
    assert_eq!(m1, m2, "manifest bytes must be deterministic for signing");
    assert!(
        m1.windows(20).any(|w| w.starts_with(b"sha256:")),
        "should contain a sha256: checksum"
    );
    let _ = sha256(b"placeholder"); // sanity: helper produces a sane string
}

/// Like [`build_signed_manifest`] but pins `created_at` so two calls
/// with identical inputs produce identical bytes. Use this for
/// byte-equality regression tests, not for the actual import path.
fn build_signed_manifest_pinned(
    signer: &KeyPair,
    name: &str,
    did: &str,
    did_json: &[u8],
    config_bytes: &[u8],
    keys_bytes: &[u8],
) -> Vec<u8> {
    let mut manifest = AgentManifest::new(name, "1.0.0", did);
    manifest.agent.description = Some("signature-test-fixture".to_string());
    manifest.agent.created_at = "2026-06-17T00:00:00+00:00".to_string();
    manifest.add_file("identity/did.json", did_json);
    manifest.add_file("config/agent.toml", config_bytes);
    manifest.add_file("identity/keys.enc", keys_bytes);

    let manifest_for_signing = AgentManifest {
        signatures: peko::registry::packaging::manifest::Signatures {
            manifest: String::new(),
            algorithm: "ed25519".to_string(),
        },
        ..manifest.clone()
    };
    let signed_bytes = manifest_for_signing.to_toml().unwrap().into_bytes();
    let signature = signer.sign(&signed_bytes);
    manifest.signatures.manifest =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
    manifest.signatures.algorithm = "ed25519".to_string();
    manifest.to_toml().unwrap().into_bytes()
}

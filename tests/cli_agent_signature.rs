//! Issue #14 — manifest signature verification on import (Principal-era).
//!
//! Builds a real `.principal` package, then exercises the unpackager's
//! signature gate with five scenarios:
//!
//!   1. green: signed manifest imports successfully
//!   2. red:   tampered manifest byte fails with signature_verification_failed
//!   3. red:   stripped signature fails (no silent fallback to "unsigned")
//!   4. red:   wrong-key signature fails (signed by author A, claims author B)
//!   5. red:   `--force` does NOT bypass signature (a security check, not a format check)
//!   6. green: `--allow-unsigned` permits an unsigned import
//!
//! These tests run without a daemon or registry — they call the
//! [`peko::registry::packaging::PrincipalUnpackager`] directly against an
//! in-memory file map, which is exactly the path the CLI's
//! `peko principal import` command eventually reaches through the daemon
//! IPC.
//!
//! ## Principal-era translation
//!
//! After the "Principal as the single actor" migration, the standalone
//! `.agent` packager/unpackager (`peko::registry::packaging::Packager` /
//! `Unpackager` / `AgentManifest`) was replaced with the equivalent
//! `.principal` surface:
//!
//! - [`peko::registry::packaging::PrincipalPackager`] (analog of `Packager`)
//! - [`peko::registry::packaging::PrincipalUnpackager`] (analog of `Unpackager`)
//! - [`peko::registry::packaging::PrincipalManifest`] (analog of `AgentManifest`)
//!
//! The byte-canonicalization contract, the `ed25519` algorithm pin, the
//! `[signature_verification_failed]` error prefix, and the
//! "force does NOT bypass signature" rule all carry over unchanged
//! (see `src/registry/packaging/principal_unpackager.rs::verify_principal_signature`).
//!
//! (Original Issue #14 referenced the legacy `.agent` push→pull→export
//! flow's CI failure surface; the same byte-canonicalization contract
//! carries over to the `.principal` push→pull→export flow exercised
//! by `packaging_integration::test_full_packaging_pipeline`.)
//!
//! See: <https://github.com/ConekoAI/peko-runtime/issues/14>

use base64::Engine;
use peko::identity::keys::KeyPair;
use peko::identity::{DIDDocument, Identity, VerificationMethod};
use peko::registry::packaging::principal_manifest::PrincipalManifest;
use peko::registry::packaging::{PrincipalImportOptions, PrincipalImportResult, PrincipalUnpackager};
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
///
/// The Principal-era shape replaces the legacy `config/agent.toml`
/// with a `config/principal.toml` body — a minimal PrincipalConfig
/// payload — and the identity files (`identity/did.json` and
/// `identity/keys.enc`) are the same as the agent-era shape.
fn build_file_contents(
    signer: &KeyPair,
    did_doc: &DIDDocument,
) -> (Vec<u8>, Vec<u8>, Vec<u8>, String) {
    let did_json = serde_json::to_vec_pretty(did_doc).unwrap();

    // A minimal PrincipalConfig-shaped TOML body. The unpackager's
    // validation doesn't pin a specific schema; it just checks that
    // the manifest's declared files exist in the package and match
    // their declared checksums. We supply a representative
    // `config/principal.toml` payload mirroring the real shape (the
    // unpackager will deserialize it into `PrincipalConfig` on import).
    let config_toml = r#"
name = "sig-test"
description = "Signature test principal"
display_name = "Signature Test Principal"

[capabilities]
tools = []
skills = []
mcps = []
agents = []
"#;
    let config_bytes = config_toml.as_bytes().to_vec();

    let key_export = signer.export();
    let keys_bytes = serde_json::to_vec(&key_export).unwrap();

    (did_json, config_bytes, keys_bytes, did_doc.id.clone())
}

/// Build a manifest signed by `signer` for the given principal name + DID.
///
/// Mirrors `PrincipalPackager`'s signing path: zero the signature,
/// re-serialize via `to_toml`, sign, base64url-no-pad encode. The
/// resulting bytes are what the unpacker reads as `manifest.toml` —
/// they include the signature because we then set the field on the
/// in-memory manifest and re-serialize.
///
/// Like the packager, this function does NOT include `manifest.toml`
/// in its own checksums map — the manifest.toml bytes are written to
/// the archive *after* signing, and the unpacker only verifies
/// checksums of files listed in the manifest.
fn build_signed_manifest(
    signer: &KeyPair,
    name: &str,
    did: &str,
    did_json: &[u8],
    config_bytes: &[u8],
    keys_bytes: &[u8],
) -> Vec<u8> {
    let mut manifest = PrincipalManifest::new(name, "1.0.0", did);
    manifest.principal.description = Some("signature-test-fixture".to_string());

    // Add real checksums for the three on-disk files.
    manifest.add_file("identity/did.json", did_json);
    manifest.add_file("config/principal.toml", config_bytes);
    manifest.add_file("identity/keys.enc", keys_bytes);

    // Reconstruct the signed bytes (signature field zeroed).
    let manifest_for_signing = PrincipalManifest {
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
    files.insert("config/principal.toml".to_string(), config_bytes.to_vec());
    files.insert("identity/keys.enc".to_string(), keys_bytes.to_vec());
    files
}

fn import_options(allow_unsigned: bool, force: bool) -> PrincipalImportOptions {
    PrincipalImportOptions {
        new_name: Some("imported-sig-test".to_string()),
        rotate_keys: false,
        import_sessions: false,
        allow_unsigned,
        force,
    }
}

async fn run_import(
    files: &HashMap<String, Vec<u8>>,
    opts: PrincipalImportOptions,
) -> Result<PrincipalImportResult, anyhow::Error> {
    // Write the files map to a tar.gz, point the PrincipalUnpackager at
    // it, and import. (The PrincipalUnpackager's
    // `import_from_files` is private; we exercise the public
    // `import` path which writes a temp `.principal` file and
    // delegates to it.)
    let temp = TempDir::new().expect("tempdir");
    let package_path = temp.path().join("test.principal");
    write_tar_gz(&package_path, files).expect("write package");

    let base_dir = TempDir::new().expect("tempdir for base");
    // PrincipalUnpackager needs both a `config_dir` (where the imported
    // `principal.toml` + `agents/` workspace land) and a `data_dir`
    // (where the principal's identity keys + sessions are stored).
    // Use the same base for both — the unpacker's tests don't read
    // back identity on disk, so colocating is fine for the signature
    // assertion surface this file exercises.
    let config_dir = base_dir.path().to_path_buf();
    let data_dir = base_dir.path().to_path_buf();
    let unpackager = PrincipalUnpackager::new(&package_path, config_dir, data_dir);
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

/// Flip one character in the principal name `sig-test` → `sxg-test`.
/// This changes the manifest content without breaking TOML parseability.
fn tamper_principal_name(manifest_bytes: &mut [u8]) {
    let needle = b"name = \"sig-test\"";
    let pos = manifest_bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("principal name line should exist in test manifest");
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
    tamper_principal_name(&mut manifest_bytes);
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
    let mut manifest = PrincipalManifest::new("sig-test", "1.0.0", &did);
    manifest.principal.description = Some("signature-test-fixture".to_string());
    manifest.add_file("identity/did.json", &did_json);
    manifest.add_file("config/principal.toml", &config_bytes);
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
    tamper_principal_name(&mut manifest_bytes);
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
    let mut manifest = PrincipalManifest::new("sig-test", "1.0.0", &did);
    manifest.principal.description = Some("signature-test-fixture".to_string());
    manifest.add_file("identity/did.json", &did_json);
    manifest.add_file("config/principal.toml", &config_bytes);
    manifest.add_file("identity/keys.enc", &keys_bytes);
    let manifest_bytes = manifest.to_toml().unwrap().into_bytes();
    let files = build_files_map(&manifest_bytes, &did_json, &config_bytes, &keys_bytes);

    // allow_unsigned: true should let the import succeed.
    let result = run_import(&files, import_options(true, false))
        .await
        .expect("allow_unsigned should permit unsigned import");
    assert_eq!(result.name, "imported-sig-test");
}

#[test]
fn manifest_round_trip_produces_identical_bytes() {
    // Issue #14 surfaced a real determinism bug: the packager signs
    // `packaging.files` in insertion order, but the registry's
    // export path rebuilds the manifest on pull and sorts the file
    // list. If the two sides disagree on the field order, the signed
    // bytes from the original export do not match the re-serialized
    // bytes on import, and signature verification fails spuriously.
    // This test exercises the serde round-trip that both the packager
    // (sign-then-write) and the registry (load-then-rebuild-then-write)
    // perform, and asserts the bytes are byte-identical. The same
    // byte-canonicalization rule carries over to the Principal-era
    // packager in `src/registry/packaging/principal_packager.rs`.
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
    let parsed: PrincipalManifest =
        PrincipalManifest::from_toml(std::str::from_utf8(&manifest_bytes).expect("manifest utf-8"))
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
    let mut manifest = PrincipalManifest::new(name, "1.0.0", did);
    manifest.principal.description = Some("signature-test-fixture".to_string());
    manifest.principal.created_at = "2026-06-17T00:00:00+00:00".to_string();
    manifest.add_file("identity/did.json", did_json);
    manifest.add_file("config/principal.toml", config_bytes);
    manifest.add_file("identity/keys.enc", keys_bytes);

    let manifest_for_signing = PrincipalManifest {
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
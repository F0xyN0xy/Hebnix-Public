//! our own CA. made on first run and dropped in the system trust store (via
//! p11-kit's `trust anchor`, Arch's replacement for certutil) so the game
//! takes our certs.

use std::path::{Path, PathBuf};
use std::process::Command;

use rcgen::{
    BasicConstraints, CertificateParams, CertificateRevocationListParams, CrlDistributionPoint,
    DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyIdMethod, KeyPair,
    KeyUsagePurpose, PKCS_RSA_SHA256, SanType, SerialNumber, date_time_ymd,
};

pub const COMMON_NAME: &str = "Hebnix Local CA"; // trust matches on this, dont change it

// leaf certs point here for the revoke check
/// CRL server ports, tried in order (Windows uses a fixed 8081). The leaf
/// certificates carry whichever one the running CRL server got.
pub const CRL_PORTS: &[u16] = &[6968, 6967, 6966, 6965, 0];
static CRL_PORT_IN_USE: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(6968);

pub fn set_crl_port(port: u16) {
    CRL_PORT_IN_USE.store(port, std::sync::atomic::Ordering::Release);
}

pub fn crl_url() -> String {
    format!(
        "http://127.0.0.1:{}/hebnix.crl",
        CRL_PORT_IN_USE.load(std::sync::atomic::Ordering::Acquire)
    )
}

pub fn dir(base_dir: &Path) -> PathBuf {
    base_dir.join("spoofer")
}

fn key_path(base_dir: &Path) -> PathBuf {
    dir(base_dir).join("ca-key.pem")
}

fn cert_path(base_dir: &Path) -> PathBuf {
    dir(base_dir).join("ca-cert.pem")
}

// p11-kit `trust anchor` wants a plain der file
fn der_path(base_dir: &Path) -> PathBuf {
    dir(base_dir).join("hebnix-ca.cer")
}

pub struct Ca {
    pub key_pem: String,
    pub cert_pem: String,
    leaf_key_pem: String,
    leaf_key_der: Vec<u8>,
}

pub struct Leaf {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

// rsa 2048, the eos client wont take an ecdsa leaf. ring cant make rsa keys so
// the rsa crate does it.
fn rsa_material() -> Result<(String, Vec<u8>), String> {
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};

    let mut rng = rand::thread_rng();
    let key = rsa::RsaPrivateKey::new(&mut rng, 2048).map_err(|e| format!("rsa keygen: {e}"))?;
    let pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| format!("rsa pem: {e}"))?
        .to_string();
    let der = key
        .to_pkcs8_der()
        .map_err(|e| format!("rsa der: {e}"))?
        .as_bytes()
        .to_vec();
    Ok((pem, der))
}

fn load_rsa_key(pem: &str) -> Result<KeyPair, String> {
    KeyPair::from_pkcs8_pem_and_sign_algo(pem, &PKCS_RSA_SHA256)
        .map_err(|e| format!("load rsa key: {e}"))
}

impl Ca {
    fn with_leaf(key_pem: String, cert_pem: String) -> Result<Self, String> {
        let (leaf_key_pem, leaf_key_der) = rsa_material()?;
        Ok(Self {
            key_pem,
            cert_pem,
            leaf_key_pem,
            leaf_key_der,
        })
    }

    pub fn sign_leaf(&self, host: &str) -> Result<Leaf, String> {
        let ca_key = load_rsa_key(&self.key_pem)?;
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, ca_key)
            .map_err(|e| format!("bad ca cert: {e}"))?;
        let leaf_key = load_rsa_key(&self.leaf_key_pem)?;

        let san = SanType::DnsName(host.try_into().map_err(|_| format!("bad host {host}"))?);
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);

        let mut params = CertificateParams::default();
        params.distinguished_name = dn;
        params.subject_alt_names = vec![san];
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.crl_distribution_points = vec![CrlDistributionPoint {
            uris: vec![crl_url()],
        }];
        params.not_before = date_time_ymd(2024, 1, 1);
        params.not_after = date_time_ymd(2034, 1, 1);

        let cert = params
            .signed_by(&leaf_key, &issuer)
            .map_err(|e| format!("leaf sign: {e}"))?;

        Ok(Leaf {
            cert_der: cert.der().to_vec(),
            key_der: self.leaf_key_der.clone(),
        })
    }

    /// empty crl, signed, same dates as the certs
    pub fn crl_der(&self) -> Result<Vec<u8>, String> {
        let ca_key = load_rsa_key(&self.key_pem)?;
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, ca_key)
            .map_err(|e| format!("bad ca cert: {e}"))?;

        let params = CertificateRevocationListParams {
            this_update: date_time_ymd(2024, 1, 1),
            next_update: date_time_ymd(2034, 1, 1),
            crl_number: SerialNumber::from(1u64),
            issuing_distribution_point: None,
            revoked_certs: Vec::new(),
            key_identifier_method: KeyIdMethod::Sha256,
        };
        let crl = params
            .signed_by(&issuer)
            .map_err(|e| format!("crl sign: {e}"))?;
        Ok(crl.der().to_vec())
    }
}

pub fn ensure(base_dir: &Path) -> Result<Ca, String> {
    let key = std::fs::read_to_string(key_path(base_dir));
    let cert = std::fs::read_to_string(cert_path(base_dir));
    if let (Ok(key_pem), Ok(cert_pem)) = (key, cert) {
        // load_rsa_key trips on an old ecdsa CA, make a new one then
        if !key_pem.trim().is_empty()
            && !cert_pem.trim().is_empty()
            && load_rsa_key(&key_pem).is_ok()
        {
            return Ca::with_leaf(key_pem, cert_pem);
        }
    }
    generate(base_dir)
}

fn generate(base_dir: &Path) -> Result<Ca, String> {
    let (key_pem, _) = rsa_material()?;
    let key_pair = load_rsa_key(&key_pem)?;

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, COMMON_NAME);
    dn.push(DnType::OrganizationName, "Hebnix");

    let mut params = CertificateParams::default();
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0)); // leaf certs only
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    // fixed dates, a wrong clock shouldnt kill the CA
    params.not_before = date_time_ymd(2024, 1, 1);
    params.not_after = date_time_ymd(2034, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| format!("self sign failed: {e}"))?;

    let cert_pem = cert.pem();
    std::fs::create_dir_all(dir(base_dir))
        .map_err(|e| format!("cant create the spoofer dir: {e}"))?;
    write(&key_path(base_dir), key_pem.as_bytes())?;
    write(&cert_path(base_dir), cert_pem.as_bytes())?;
    write(&der_path(base_dir), &cert.der().to_vec())?;

    tracing::info!("minted a new spoofer CA in {}", dir(base_dir).display());
    Ca::with_leaf(key_pem, cert_pem)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cant write {}: {e}", path.display()))
}

/// make it if missing, then trust it system-wide via p11-kit. needs root -
/// callers go through `spoofer::run_privileged` (a one-shot pkexec call).
pub fn install(base_dir: &Path) -> Result<(), String> {
    if is_current_installed(base_dir) {
        return Ok(());
    }
    ensure(base_dir)?;
    let der = der_path(base_dir);
    if !der.is_file() {
        return Err(format!("{} is missing", der.display()));
    }
    let _ = uninstall(base_dir); // kick out any older CA of ours first
    trust(&["anchor", "--store", &der.to_string_lossy()]).map(|_| ())
}

/// one trusted anchor carrying our label, as printed by `trust dump`
struct Anchor {
    uri: String,
    pem_body: String,
}

fn pem_body(pem: &str) -> String {
    pem.lines()
        .filter(|line| !line.starts_with("-----"))
        .flat_map(|line| line.chars().filter(|c| !c.is_whitespace()))
        .collect()
}

/// every anchor labelled COMMON_NAME. a CA re-minted after the old one was
/// trusted leaves the old anchor behind under the same label, so matching on
/// the label alone is not enough to know *our* cert is trusted.
fn parse_anchors(dump: &str) -> Vec<Anchor> {
    let label = format!("label: \"{COMMON_NAME}\"");
    let mut anchors = Vec::new();
    let mut uri: Option<&str> = None;
    let mut ours = false;
    let mut pem = String::new();
    let mut in_pem = false;
    for line in dump.lines() {
        if line.starts_with("# pkcs11:") {
            uri = Some(&line[2..]);
            ours = false;
            pem.clear();
            in_pem = false;
        } else if line.trim() == label {
            ours = true;
        } else if line.starts_with("-----BEGIN CERTIFICATE-----") {
            in_pem = true;
            pem.clear();
        } else if line.starts_with("-----END CERTIFICATE-----") {
            in_pem = false;
            if let (true, Some(uri)) = (ours, uri.take()) {
                anchors.push(Anchor {
                    uri: uri.to_string(),
                    pem_body: pem_body(&pem),
                });
            }
        } else if in_pem {
            pem.push_str(line);
        }
    }
    anchors
}

fn installed_anchors() -> Result<Vec<Anchor>, String> {
    trust(&["dump", "--filter=ca-anchors"]).map(|out| parse_anchors(&out))
}

/// is our current CA trusted. compares the trusted cert itself, not just the
/// label, so a stale anchor from an older CA does not count.
pub fn is_current_installed(base_dir: &Path) -> bool {
    let Ok(cert_pem) = std::fs::read_to_string(cert_path(base_dir)) else {
        return false;
    };
    let ours = pem_body(&cert_pem);
    installed_anchors().is_ok_and(|anchors| anchors.iter().any(|a| a.pem_body == ours))
}

/// remove every anchor carrying our label, including ones from an older CA
/// that no longer matches the files on disk.
pub fn uninstall(_base_dir: &Path) -> Result<(), String> {
    for anchor in installed_anchors()? {
        trust(&["anchor", "--remove", &anchor.uri])?;
    }
    Ok(())
}

fn trust(args: &[&str]) -> Result<String, String> {
    let out = Command::new("trust")
        .args(args)
        .output()
        .map_err(|e| format!("cant run `trust` (is p11-kit installed?): {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    if out.status.success() {
        return Ok(stdout.into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msg = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };
    Err(format!("trust: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_a_ca_and_reuses_it() {
        let base = std::env::temp_dir().join("hebnix_ca_test");
        let _ = std::fs::remove_dir_all(&base);

        let ca = ensure(&base).expect("mint failed");
        assert!(ca.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(ca.key_pem.contains("PRIVATE KEY"));
        assert!(der_path(&base).is_file());

        // second call loads it, doesnt make a new one
        let again = ensure(&base).expect("reload failed");
        assert_eq!(ca.cert_pem, again.cert_pem);

        let _ = std::fs::remove_dir_all(&base);
    }

    // the leaf has to carry the crl url
    #[test]
    fn leaf_carries_crl_distribution_point() {
        let base = std::env::temp_dir().join("hebnix_ca_crl");
        let _ = std::fs::remove_dir_all(&base);
        let ca = ensure(&base).expect("mint failed");
        let leaf = ca.sign_leaf("config.psynet.gg").expect("leaf sign failed");
        let url = crl_url();
        let needle = url.as_bytes();
        assert!(
            leaf.cert_der.windows(needle.len()).any(|w| w == needle),
            "leaf missing the CRL distribution point url"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn crl_generates() {
        let base = std::env::temp_dir().join("hebnix_ca_crlgen");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(dir(&base)).unwrap();
        let ca = ensure(&base).expect("mint failed");

        let der = ca.crl_der().expect("crl gen failed");
        assert!(!der.is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_only_our_anchors() {
        let dump = "\
# pkcs11:id=%01;type=cert
[p11-kit-object-v1]
label: \"Other CA\"
-----BEGIN CERTIFICATE-----
AAAA
-----END CERTIFICATE-----

# pkcs11:id=%A2%46;type=cert
[p11-kit-object-v1]
label: \"Hebnix Local CA\"
trusted: true
-----BEGIN CERTIFICATE-----
MIID
KTCC
-----END CERTIFICATE-----
";
        let anchors = parse_anchors(dump);
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].uri, "pkcs11:id=%A2%46;type=cert");
        assert_eq!(anchors[0].pem_body, "MIIDKTCC");
    }

    // a CA we never installed must not read as installed
    #[test]
    fn uninstalled_ca_is_not_reported_installed() {
        let base = std::env::temp_dir().join("hebnix_ca_notinstalled");
        let _ = std::fs::remove_dir_all(&base);
        ensure(&base).expect("mint failed");

        assert!(!is_current_installed(&base));

        let _ = std::fs::remove_dir_all(&base);
    }
}

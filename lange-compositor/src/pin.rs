//! PIN da tela de bloqueio: PBKDF2-HMAC-SHA256 com salt aleatório, arquivo 0600 do compositor.
//! PIN de 4-8 dígitos tem poucas combinações: o que protege é o arquivo só do compositor + a espera
//! após erros (shell). O hash evita guardar o PIN em claro.
use std::{fs, io::Read};

const ITERS: u32 = 20_000; // ponytail: baixo p/ o QEMU (TCG); subir no aparelho real

#[derive(Debug, Clone, PartialEq)]
pub struct Pin { salt: [u8; 16], hash: [u8; 32] }

fn derive(pin: &str, salt: &[u8; 16]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(pin.as_bytes(), salt, ITERS, &mut out);
    out
}

impl Pin {
    pub fn new(pin: &str) -> std::io::Result<Pin> {
        let mut salt = [0u8; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
        Ok(Pin { salt, hash: derive(pin, &salt) })
    }

    /// Comparação em tempo constante.
    pub fn check(&self, pin: &str) -> bool {
        derive(pin, &self.salt).iter().zip(self.hash).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
    }

    pub fn load(path: &str) -> Option<Pin> {
        let t = fs::read_to_string(path).ok()?;
        let mut it = t.split_whitespace();
        let (salt, hash) = (unhex(it.next()?)?, unhex(it.next()?)?);
        Some(Pin { salt: salt.try_into().ok()?, hash: hash.try_into().ok()? })
    }

    /// Grava 0600 por tmp + rename (sem arquivo pela metade se faltar luz).
    pub fn save(&self, path: &str) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = format!("{path}.tmp");
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        writeln!(f, "{} {}", hex(&self.salt), hex(&self.hash))?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    }
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pin_roundtrip() {
        let p = Pin::new("1234").unwrap();
        assert!(p.check("1234") && !p.check("1235") && !p.check(""));
        let path = std::env::temp_dir().join(format!("lange-pin-{}", std::process::id()));
        let path = path.to_str().unwrap();
        p.save(path).unwrap();
        assert_eq!(Pin::load(path), Some(p));
        let _ = fs::remove_file(path);
    }
}

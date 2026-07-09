//! In-memory secret holder (architecture D2 / specification §3, §10):
//! keys never hit argv or disk, are redacted from Debug output, and the
//! backing bytes are zeroed on drop.

use std::fmt;

pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(value: String) -> Self {
        Secret(value.into_bytes())
    }

    /// Borrow the secret for the request being built. Callers must not copy
    /// it into long-lived storage.
    pub fn expose(&self) -> &str {
        // Constructed from a String, so the bytes are valid UTF-8.
        std::str::from_utf8(&self.0).expect("secret constructed from String")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Volatile writes so the zeroing cannot be optimized away as a
        // dead store on the soon-to-be-freed buffer.
        #[allow(unsafe_code)]
        for byte in &mut self.0 {
            // SAFETY: `byte` is a valid, exclusive reference into owned memory.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(«redacted»)")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn exposes_original_value() {
        let s = Secret::new("sk-test".to_owned());
        assert_eq!(s.expose(), "sk-test");
    }

    #[test]
    fn debug_never_prints_the_value() {
        let s = Secret::new("sk-test".to_owned());
        assert_eq!(format!("{s:?}"), "Secret(«redacted»)");
    }
}

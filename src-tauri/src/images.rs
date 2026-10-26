//! "Always load images from this sender": the senders whose remote images are shown without asking.
//! Everyone else's remote images stay blocked until "Load images" is pressed for that one message.

use crate::storage;
use anyhow::{Result, bail};
use rusqlite::OptionalExtension;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS image_senders (address TEXT PRIMARY KEY, added_at INTEGER NOT NULL)";

/// The stored form of an address: trimmed and lower-case, and plausibly one address.
fn normalise(address: &str) -> Result<String> {
    let a = address.trim().to_lowercase();
    if a.len() > 320 || a.matches('@').count() != 1 || a.starts_with('@') || a.ends_with('@') || a.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("That isn't an email address.");
    }
    Ok(a)
}

pub fn allowed(address: &str) -> Result<bool> {
    let Ok(a) = normalise(address) else { return Ok(false) };
    storage::with_db(|db| {
        db.execute_batch(TABLE)?;
        Ok(db.query_row("SELECT 1 FROM image_senders WHERE address=?1", [a], |_| Ok(())).optional()?.is_some())
    })
}

pub fn set(address: &str, allow: bool) -> Result<()> {
    let a = normalise(address)?;
    storage::with_db(|db| {
        db.execute_batch(TABLE)?;
        if allow {
            db.execute("INSERT OR IGNORE INTO image_senders(address, added_at) VALUES(?1, strftime('%s','now'))", [a])?;
        } else {
            db.execute("DELETE FROM image_senders WHERE address=?1", [a])?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senders_can_be_trusted_and_untrusted() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("gm-img-{}", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg")) };
        storage::initialize(tmp.join("vault").to_str().unwrap()).unwrap();

        assert!(!allowed("news@shop.example").unwrap(), "blocked by default");
        set(" News@Shop.Example ", true).unwrap();
        assert!(allowed("news@shop.example").unwrap(), "case and blanks don't matter");
        assert!(!allowed("other@shop.example").unwrap(), "only that sender");
        set("news@shop.example", true).unwrap(); // twice is fine
        set("NEWS@shop.example", false).unwrap();
        assert!(!allowed("news@shop.example").unwrap());
        for bad in ["", "no-at-sign", "a@b@c", "@x.com", "a b@c.com", "a\n@c.com"] {
            assert!(set(bad, true).is_err(), "{bad:?}");
            assert!(!allowed(bad).unwrap());
        }
    }
}

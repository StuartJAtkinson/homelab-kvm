//! On-disk state: this machine's permanent id and the last known layout.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use kvm_core::{Layout, NodeId};
use tracing::warn;

use crate::{Event, Identity, KvmNode};

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// `dir` defaults to the platform config dir (`%APPDATA%\homelab-kvm`,
    /// `~/.config/homelab-kvm`).
    pub fn open(dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let dir = match dir {
            Some(d) => d,
            None => dirs::config_dir().context("no config directory")?.join("homelab-kvm"),
        };
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn node_id(&self) -> anyhow::Result<NodeId> {
        let path = self.dir.join("node-id");
        if let Ok(s) = fs::read_to_string(&path) {
            if let Ok(id) = s.trim().parse() {
                return Ok(id);
            }
            warn!("{} is corrupt, generating a new id", path.display());
        }
        let id = NodeId::new();
        fs::write(&path, id.to_string())?;
        Ok(id)
    }

    /// This machine's Noise key pair, created on first run. Stored as two hex
    /// lines (private, public); readable only by the owner on Unix.
    pub fn identity(&self) -> anyhow::Result<Identity> {
        let path = self.dir.join("identity.key");
        if let Ok(s) = fs::read_to_string(&path) {
            let mut lines = s.lines();
            let parsed = (|| {
                let private = hex::decode(lines.next()?.trim()).ok()?.try_into().ok()?;
                let public = hex::decode(lines.next()?.trim()).ok()?.try_into().ok()?;
                Some(Identity { private, public })
            })();
            // Never silently replace a key: peers would see a stranger.
            return parsed.with_context(|| format!("{} is corrupt; delete it to re-pair this machine", path.display()));
        }
        let id = Identity::generate();
        write_private(&path, format!("{}\n{}\n", hex::encode(id.private), hex::encode(id.public)).as_bytes())?;
        Ok(id)
    }

    pub fn load_layout(&self) -> Layout {
        let path = self.dir.join("layout.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                warn!("ignoring unreadable {}: {e}", path.display());
                Layout::default()
            }),
            Err(_) => Layout::default(),
        }
    }

    pub fn save_layout(&self, layout: &Layout) -> anyhow::Result<()> {
        let path = self.dir.join("layout.json");
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(layout)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Save the layout whenever it changes, until the node goes away.
    pub fn persist(self, node: &KvmNode) {
        let mut rx = node.subscribe();
        let initial = node.layout();
        tokio::spawn(async move {
            if let Err(e) = self.save_layout(&initial) {
                warn!("saving layout: {e:#}");
            }
            loop {
                match rx.recv().await {
                    Ok(Event::LayoutChanged(l)) => {
                        if let Err(e) = self.save_layout(&l) {
                            warn!("saving layout: {e:#}");
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(data)
}

/// %APPDATA% is already per-user on Windows.
#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("kvm-store-{}", NodeId::new()));
        let store = Store::open(Some(dir.clone())).unwrap();
        let a = store.identity().unwrap();
        let b = store.identity().unwrap();
        assert_eq!((a.private, a.public), (b.private, b.public));
        fs::write(dir.join("identity.key"), "junk").unwrap();
        assert!(store.identity().is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

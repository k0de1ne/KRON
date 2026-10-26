//! The agent's Linux on Android: an Ubuntu rootfs run through proot, so the
//! agent gets apt, compilers and a normal filesystem inside the app sandbox.
//! Active when the app passes KRON_NATIVE_DIR with libproot.so in it.

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const ROOTFS_URL: &str =
    "https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/ubuntu-base-24.04.3-base-arm64.tar.gz";
const ROOTFS_SHA256: &str = "7b2dced6dd56ad5e4a813fa25c8de307b655fdabc6ea9213175a92c48dabb048";
const DISTRO: &str = "Ubuntu 24.04";
/// The phone's shared storage, used as /shared when the app may write there.
const SDCARD_DIR: &str = "/storage/emulated/0/kron";

pub struct Sandbox {
    native: PathBuf,
    pub rootfs: PathBuf,
    pub shared: PathBuf,
    tmp: PathBuf,
}

pub fn get() -> Option<&'static Sandbox> {
    static S: OnceLock<Option<Sandbox>> = OnceLock::new();
    S.get_or_init(|| {
        let native = PathBuf::from(std::env::var_os("KRON_NATIVE_DIR")?);
        if !native.join("libproot.so").exists() {
            return None;
        }
        let home = crate::config::user_home();
        let shared = match std::fs::create_dir_all(SDCARD_DIR)
            .and_then(|_| std::fs::write(Path::new(SDCARD_DIR).join(".kron"), ""))
        {
            Ok(()) => PathBuf::from(SDCARD_DIR),
            Err(_) => home.join("shared"),
        };
        let tmp = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| home.join("tmp"));
        Some(Sandbox { native, rootfs: home.join("linux"), shared, tmp })
    })
    .as_ref()
}

impl Sandbox {
    /// Where the agent starts: /root inside the rootfs.
    pub fn home(&self) -> PathBuf {
        self.rootfs.join("root")
    }

    /// A path as the agent sees it (/root/x, /shared/x) → the real file.
    pub fn host_path(&self, p: &Path) -> PathBuf {
        match p.strip_prefix("/shared") {
            Ok(rest) => self.shared.join(rest),
            Err(_) => self.rootfs.join(p.strip_prefix("/").unwrap_or(p)),
        }
    }

    /// The real file → the path the agent sees.
    pub fn guest_path(&self, p: &Path) -> PathBuf {
        if let Ok(rest) = p.strip_prefix(&self.shared) {
            return Path::new("/shared").join(rest);
        }
        match p.strip_prefix(&self.rootfs) {
            Ok(rest) => Path::new("/").join(rest),
            Err(_) => p.to_path_buf(),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{DISTRO} (aarch64) in a proot container on an Android phone. You are root; install anything with \
             `apt-get install -y` (run `apt-get update` first). /shared is a folder shared with the phone \
             ({}): put files the user should get there.",
            if self.shared.starts_with(SDCARD_DIR) { "Internal storage/kron" } else { "app storage" }
        )
    }

    /// proot running `sh -c cmd` inside the rootfs.
    pub fn command(&self, cmd: &str, cwd: &Path) -> Command {
        let mut c = Command::new(self.native.join("libproot.so"));
        c.env("PROOT_LOADER", self.native.join("libproot-loader.so"))
            .env("PROOT_TMP_DIR", &self.tmp)
            .env("LD_LIBRARY_PATH", &self.native)
            .arg("-0")
            .arg("--link2symlink")
            .arg("--kill-on-exit")
            .arg("--sysvipc")
            .arg("-L")
            .arg("-r")
            .arg(&self.rootfs);
        for b in ["/dev", "/proc", "/sys"] {
            c.arg("-b").arg(b);
        }
        c.arg("-b").arg(format!("{}:/dev/shm", self.tmp.display()));
        c.arg("-b").arg(format!("{}:/shared", self.shared.display()));
        c.arg("-w").arg(self.guest_path(cwd));
        c.args(["/usr/bin/env", "-i", "HOME=/root", "USER=root", "LANG=C.UTF-8", "TERM=xterm-256color", "TMPDIR=/tmp"])
            .arg("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
            .args(["DEBIAN_FRONTEND=noninteractive", "/bin/bash", "-c", cmd]);
        c
    }

    /// Download and unpack the rootfs on first use; later calls return at once.
    pub async fn ready(&self) -> Result<()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let marker = self.rootfs.join(".kron-ready");
        if marker.exists() {
            return Ok(());
        }
        let _guard = LOCK.lock().await;
        if marker.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.shared)?;
        std::fs::create_dir_all(&self.tmp)?;
        eprintln!("sandbox: downloading {DISTRO}");
        let archive = self.tmp.join("rootfs.tar.gz");
        download(ROOTFS_URL, &archive, ROOTFS_SHA256).await?;
        eprintln!("sandbox: unpacking into {}", self.rootfs.display());
        let part = self.rootfs.with_extension("part");
        let (p, a) = (part.clone(), archive.clone());
        tokio::task::spawn_blocking(move || unpack(&a, &p)).await??;
        std::fs::remove_file(&archive).ok();
        setup(&part)?;
        if self.rootfs.exists() {
            std::fs::remove_dir_all(&self.rootfs)?;
        }
        std::fs::rename(&part, &self.rootfs)?;
        std::fs::write(&marker, DISTRO)?;
        eprintln!("sandbox: ready");
        Ok(())
    }
}

async fn download(url: &str, to: &Path, sha256: &str) -> Result<()> {
    let resp = crate::llm::http().get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(to).await?;
    let mut hash = Sha256::new();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        hash.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != sha256 {
        bail!("{url}: checksum mismatch");
    }
    Ok(())
}

/// Unpack a .tar.gz. Android forbids hard links in app storage, so those
/// become copies; directory modes are applied last so read-only ones fill.
fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).with_context(|| format!("cannot clear {}", dest.display()))?;
    }
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
    let mut tar = tar::Archive::new(gz);
    let mut dirs = Vec::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.into_owned();
        if rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            continue;
        }
        let path = dest.join(&rel);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&path)?;
                dirs.push((path, entry.header().mode()?));
            }
            tar::EntryType::Link => {
                let target = dest.join(entry.link_name()?.context("hard link without target")?);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&target, &path)?;
            }
            tar::EntryType::Regular | tar::EntryType::Symlink | tar::EntryType::Continuous => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                entry.unpack(&path)?;
            }
            _ => {} // device nodes, fifos: proot binds the real /dev
        }
    }
    #[cfg(unix)]
    for (dir, mode) in dirs.into_iter().rev() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode | 0o700)).ok();
    }
    Ok(())
}

/// What a bare rootfs needs to work under proot.
fn setup(rootfs: &Path) -> Result<()> {
    let w = |rel: &str, text: &str| -> Result<()> {
        let p = rootfs.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::remove_file(&p).ok(); // resolv.conf is a dangling symlink
        std::fs::write(&p, text)?;
        Ok(())
    };
    w("etc/resolv.conf", "nameserver 8.8.8.8\nnameserver 1.1.1.1\n")?;
    w("etc/hosts", "127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n")?;
    // apt drops privileges to _apt, which proot's fake root cannot do.
    w("etc/apt/apt.conf.d/01kron", "APT::Sandbox::User \"root\";\n")?;
    std::fs::create_dir_all(rootfs.join("root"))?;
    std::fs::create_dir_all(rootfs.join("shared"))?; // mount point for -b shared:/shared
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(rootfs.join("tmp"), std::fs::Permissions::from_mode(0o1777)).ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_both_ways() {
        let s = Sandbox {
            native: "/n".into(),
            rootfs: "/data/app/linux".into(),
            shared: "/sdcard/kron".into(),
            tmp: "/t".into(),
        };
        assert_eq!(s.host_path(Path::new("/root/a.txt")), Path::new("/data/app/linux/root/a.txt"));
        assert_eq!(s.host_path(Path::new("/shared/x")), Path::new("/sdcard/kron/x"));
        assert_eq!(s.guest_path(Path::new("/data/app/linux/root")), Path::new("/root"));
        assert_eq!(s.guest_path(Path::new("/sdcard/kron/p")), Path::new("/shared/p"));
    }

    #[cfg(unix)]
    #[test]
    fn unpack_copies_hard_links() {
        let d = std::env::temp_dir().join(format!("kron-sb-{}", crate::types::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        let archive = d.join("a.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(std::fs::File::create(&archive).unwrap(), Default::default());
            let mut b = tar::Builder::new(gz);
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Directory);
            h.set_mode(0o555);
            h.set_size(0);
            b.append_data(&mut h, "./usr/", std::io::empty()).unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_mode(0o755);
            h.set_size(2);
            b.append_data(&mut h, "./usr/a", &b"hi"[..]).unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Link);
            h.set_size(0);
            b.append_link(&mut h, "./usr/b", "./usr/a").unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let root = d.join("root");
        unpack(&archive, &root).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("usr/b")).unwrap(), "hi");
        std::fs::set_permissions(root.join("usr"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }
}

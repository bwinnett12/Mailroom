// src/coffer_push.rs
//
// Copy this machine's coffer bytes to another place: Locomotive over ssh,
// or a mounted drive. A manual backup, run when you choose (on ethernet,
// say), until phase 8's replication replaces it.
//
//   mailroom coffer status [--to DEST]                  what's only here
//   mailroom coffer push   [--to DEST] [--dry-run] [--max-size 100M]
//
// DEST is `host:/path` (ssh, scp-style; `user@host:/path` works) or a
// local path. Default: MAILROOM_COFFER_REMOTE. It is the destination's
// contents folder, e.g. locomotive:/home/tarobutter/Orchard/85_coffer/85-B_contents.
//
// Integrity rule, whichever way the bytes travel: a blob takes its name at
// the destination only after it has been hashed there and matches. A copy
// that arrives damaged, or a blob that had already rotted on this machine,
// is deleted at the far end and reported, never kept under a name that
// lies. (rsync alone checks that the copy matches the source; it would
// happily replicate a rotted source.)
//
// Transport, to a remote:
//   rsync, when both ends have it: missing blobs go into a hidden staging
//     folder, <dest>/.incoming/, in one stream, and an interrupted big file
//     resumes where it stopped. Then one ssh call hashes each arrival and
//     moves the good ones into place.
//   plain ssh otherwise (needs only coreutils at the far end): each blob
//     streams to a hidden temp file, is hashed there, then renamed.
// MAILROOM_NO_RSYNC=1 forces plain ssh. To a local path (a mounted drive),
// the store's own verified copy is used.
//
// Blobs already at the destination are skipped; smallest go first, so an
// interrupted push has sent as much as it could. `--max-size` sends only
// files up to that size (books now, movies later on a better connection).
//
// Only bytes move. Pointers travel with the vault (git); ledgers never
// leave their machine.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq)]
pub enum Dest {
    Local(PathBuf),
    Remote { host: String, path: String },
}

impl Dest {
    /// `host:/path` or `user@host:path` is remote; anything else (including
    /// `/abs/path`, `./rel`, `C:\…`-free unix paths) is local.
    pub fn parse(s: &str) -> Dest {
        let looks_remote = !s.starts_with('/') && !s.starts_with('.') && s.contains(':');
        match s.split_once(':') {
            Some((host, path)) if looks_remote && !host.is_empty() && !host.contains('/') => Dest::Remote {
                host: host.to_string(),
                path: if path.is_empty() { ".".to_string() } else { path.to_string() },
            },
            _ => Dest::Local(PathBuf::from(s)),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Dest::Local(p) => p.display().to_string(),
            Dest::Remote { host, path } => format!("{host}:{path}"),
        }
    }
}

/// One blob in this coffer.
#[derive(Debug, Clone)]
pub struct Blob {
    pub hash: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// Every blob in a contents folder (<aa>/<sha256>), smallest first.
pub fn local_blobs(contents: &Path) -> anyhow::Result<Vec<Blob>> {
    let mut found = Vec::new();
    if !contents.is_dir() {
        return Ok(found);
    }
    for fan in std::fs::read_dir(contents)? {
        let fan = fan?;
        if !fan.file_type()?.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(fan.path())? {
            let f = f?;
            let name = f.file_name().to_string_lossy().to_string();
            if f.file_type()?.is_file() && crate::blobs::is_hash(&name) {
                found.push(Blob { bytes: f.metadata()?.len(), path: f.path(), hash: name });
            }
        }
    }
    found.sort_by(|a, b| a.bytes.cmp(&b.bytes).then_with(|| a.hash.cmp(&b.hash)));
    Ok(found)
}

/// The hashes already at the destination.
pub fn remote_hashes(dest: &Dest) -> anyhow::Result<HashSet<String>> {
    match dest {
        Dest::Local(p) => Ok(local_blobs(p)?.into_iter().map(|b| b.hash).collect()),
        Dest::Remote { host, path } => {
            // A missing folder is an empty coffer, not an error.
            let script = format!("cd {} 2>/dev/null && find . -mindepth 2 -maxdepth 2 -type f || true", quote(path));
            let out = ssh(host).arg(script).output()?;
            if !out.status.success() {
                anyhow::bail!("ssh {host} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.rsplit('/').next())
                .filter(|n| crate::blobs::is_hash(n))
                .map(String::from)
                .collect())
        }
    }
}

#[derive(Debug, Default)]
pub struct Status {
    pub local: usize,
    pub local_bytes: u64,
    /// Blobs that exist only on this machine.
    pub only_here: Vec<Blob>,
    /// Blobs at the destination that this machine doesn't hold (fine:
    /// this coffer is partial).
    pub only_there: usize,
}

pub fn status(contents: &Path, dest: &Dest) -> anyhow::Result<Status> {
    let local = local_blobs(contents)?;
    let there = remote_hashes(dest)?;
    let here: HashSet<&str> = local.iter().map(|b| b.hash.as_str()).collect();
    Ok(Status {
        local: local.len(),
        local_bytes: local.iter().map(|b| b.bytes).sum(),
        only_there: there.iter().filter(|h| !here.contains(h.as_str())).count(),
        only_here: local.into_iter().filter(|b| !there.contains(&b.hash)).collect(),
    })
}

#[derive(Debug, Default)]
pub struct Pushed {
    /// "rsync", "ssh" or "local copy".
    pub transport: String,
    pub sent: Vec<Blob>,
    /// Bigger than --max-size: left for another time.
    pub held_back: Vec<Blob>,
    pub failed: Vec<(Blob, String)>,
}

/// Send every blob the destination lacks (up to `max_size`, if given).
/// `on_sent` is called after each blob, for progress output.
pub fn push(
    contents: &Path,
    dest: &Dest,
    max_size: Option<u64>,
    dry_run: bool,
    mut on_sent: impl FnMut(&Blob, Option<&str>),
) -> anyhow::Result<Pushed> {
    let st = status(contents, dest)?;
    let mut out = Pushed::default();
    let mut to_send = Vec::new();
    for blob in st.only_here {
        if max_size.is_some_and(|m| blob.bytes > m) {
            out.held_back.push(blob);
        } else {
            to_send.push(blob);
        }
    }

    let use_rsync = match dest {
        Dest::Remote { host, .. } => rsync_usable(host),
        Dest::Local(_) => false,
    };
    out.transport = match (dest, use_rsync) {
        (Dest::Local(_), _) => "local copy",
        (_, true) => "rsync",
        (_, false) => "ssh",
    }
    .to_string();

    if dry_run || to_send.is_empty() {
        out.sent = to_send;
        return Ok(out);
    }

    if let (Dest::Remote { host, path }, true) = (dest, use_rsync) {
        push_rsync(contents, host, path, to_send, &mut out, &mut on_sent)?;
        return Ok(out);
    }

    for blob in to_send {
        match send(&blob, dest) {
            Ok(()) => {
                on_sent(&blob, None);
                out.sent.push(blob);
            }
            Err(e) => {
                let msg = format!("{e:#}");
                on_sent(&blob, Some(&msg));
                out.failed.push((blob, msg));
            }
        }
    }
    Ok(out)
}

/// rsync here and at `host`, and not switched off.
fn rsync_usable(host: &str) -> bool {
    if std::env::var("MAILROOM_NO_RSYNC").is_ok_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    let here = Command::new("rsync").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status();
    if !here.is_ok_and(|s| s.success()) {
        return false;
    }
    ssh(host)
        .arg("command -v rsync >/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// rsync the blobs into <path>/.incoming/, then hash each at the far end and
/// move the good ones into place. Files already complete in .incoming (from
/// an interrupted push) are skipped by rsync; a partial one resumes.
fn push_rsync(
    contents: &Path,
    host: &str,
    path: &str,
    blobs: Vec<Blob>,
    out: &mut Pushed,
    on_sent: &mut impl FnMut(&Blob, Option<&str>),
) -> anyhow::Result<()> {
    let root = path.trim_end_matches('/');
    let incoming = format!("{root}/.incoming");
    let list: String = blobs.iter().map(|b| format!("{}/{}\n", &b.hash[..2], b.hash)).collect();

    // 0. rsync creates only the last folder of the destination; make the
    // whole staging path first.
    let mk = ssh(host).arg(format!("mkdir -p {}", quote(&incoming))).output()?;
    if !mk.status.success() {
        anyhow::bail!("could not create {incoming} at {host}: {}", String::from_utf8_lossy(&mk.stderr).trim());
    }

    // 1. Transfer. -s keeps paths with spaces intact; --partial-dir keeps a
    // cut-off file to resume; files land writable so step 2 can move them.
    let mut child = Command::new("rsync")
        .args(["-rt", "-s", "--partial", "--partial-dir=.rsync-partial", "--chmod=F644,D755", "--files-from=-"])
        .arg("-e")
        .arg(ssh_command_line())
        .arg(format!("{}/", contents.display()))
        .arg(format!("{host}:{incoming}/"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped").write_all(list.as_bytes())?;
    let rsync = child.wait_with_output()?;
    // Even if rsync was interrupted, place whatever arrived whole.
    let rsync_error = (!rsync.status.success())
        .then(|| format!("rsync stopped ({}): {}", rsync.status, String::from_utf8_lossy(&rsync.stderr).trim()));

    // 2. Verify and place, at the far end. One line per hash: ok/bad/missing.
    let script = format!(
        "cd {inc} 2>/dev/null || exit 0\n\
         while read h; do\n\
           fan=$(printf %.2s \"$h\")\n\
           if [ ! -f \"$fan/$h\" ]; then echo \"missing $h\"; continue; fi\n\
           got=$(sha256sum \"$fan/$h\" | cut -c1-64)\n\
           if [ \"$got\" = \"$h\" ]; then\n\
             mkdir -p {root}/\"$fan\" && chmod 444 \"$fan/$h\" 2>/dev/null; mv -f \"$fan/$h\" {root}/\"$fan/$h\" && echo \"ok $h\"\n\
           else rm -f \"$fan/$h\"; echo \"bad $h $got\"; fi\n\
         done",
        inc = quote(&incoming),
        root = quote(root),
    );
    let hashes: String = blobs.iter().map(|b| format!("{}\n", b.hash)).collect();
    let mut child = ssh(host).arg(script).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    child.stdin.take().expect("piped").write_all(hashes.as_bytes())?;
    let placed = child.wait_with_output()?;
    if !placed.status.success() {
        anyhow::bail!("checking arrivals at {host} failed: {}", String::from_utf8_lossy(&placed.stderr).trim());
    }

    let mut verdict: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&placed.stdout).lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("ok"), Some(h)) => {
                verdict.insert(h.to_string(), None);
            }
            (Some("bad"), Some(h)) => {
                let got = parts.next().unwrap_or("?");
                verdict.insert(h.to_string(), Some(format!("bytes arrived as {got}; deleted at {host}")));
            }
            _ => {}
        }
    }
    for blob in blobs {
        match verdict.remove(&blob.hash) {
            Some(None) => {
                on_sent(&blob, None);
                out.sent.push(blob);
            }
            Some(Some(why)) => {
                on_sent(&blob, Some(&why));
                out.failed.push((blob, why));
            }
            None => {
                let why = rsync_error.clone().unwrap_or_else(|| "did not arrive".to_string());
                on_sent(&blob, Some(&why));
                out.failed.push((blob, why));
            }
        }
    }
    Ok(())
}

fn send(blob: &Blob, dest: &Dest) -> anyhow::Result<()> {
    let fan = &blob.hash[..2];
    match dest {
        Dest::Local(root) => {
            // The verified copy the local store already uses.
            crate::blobs::put_blocking(root, &blob.path, &blob.hash)?;
            Ok(())
        }
        Dest::Remote { host, path } => {
            let dir = format!("{}/{fan}", path.trim_end_matches('/'));
            let (tmp, fin) = (format!("{dir}/.{}.tmp", blob.hash), format!("{dir}/{}", blob.hash));
            // Hash at the far end before the file takes its name; exit 3
            // and remove the temp file if the bytes don't match.
            let script = format!(
                "set -e; mkdir -p {d}; cat > {t}; \
                 h=$(sha256sum {t} | cut -c1-64); \
                 if [ \"$h\" != {hash} ]; then rm -f {t}; echo \"bytes arrived as $h\" >&2; exit 3; fi; \
                 chmod 444 {t} 2>/dev/null || true; mv -f {t} {f}",
                d = quote(&dir),
                t = quote(&tmp),
                f = quote(&fin),
                hash = blob.hash,
            );
            let mut child = ssh(host).arg(script).stdin(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
            {
                let mut stdin = child.stdin.take().expect("piped");
                let mut file = std::fs::File::open(&blob.path)?;
                // A closed pipe means the far end gave up; its exit status says why.
                let _ = std::io::copy(&mut file, &mut stdin).and_then(|_| stdin.flush());
            }
            let out = child.wait_with_output()?;
            if !out.status.success() {
                anyhow::bail!(
                    "not stored at {host} ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(())
        }
    }
}

const SSH_OPTIONS: [&str; 8] = [
    "-o", "BatchMode=yes",
    "-o", "ControlMaster=auto",
    "-o", "ControlPath=/tmp/mailroom-ssh-%r@%h:%p",
    "-o", "ControlPersist=60",
];

fn ssh_program() -> String {
    std::env::var("MAILROOM_SSH").unwrap_or_else(|_| "ssh".into())
}

/// ssh with connection sharing, so many blobs cost one handshake.
/// MAILROOM_SSH replaces the ssh command (tests use a stand-in).
fn ssh(host: &str) -> Command {
    let mut c = Command::new(ssh_program());
    c.args(SSH_OPTIONS).arg(host);
    c
}

/// The same ssh, as one line for rsync's -e.
fn ssh_command_line() -> String {
    std::iter::once(ssh_program()).chain(SSH_OPTIONS.iter().map(|s| s.to_string())).collect::<Vec<_>>().join(" ")
}

/// Single-quote for a POSIX shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// "100M" / "2G" / "512k" / "1000" → bytes.
pub fn parse_size(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&s[..s.len() - 1], 1u64 << 10),
        Some('M') => (&s[..s.len() - 1], 1 << 20),
        Some('G') => (&s[..s.len() - 1], 1 << 30),
        Some('T') => (&s[..s.len() - 1], 1 << 40),
        _ => (s, 1),
    };
    let n: f64 = num.trim().parse().map_err(|_| anyhow::anyhow!("not a size: {s:?} (try 100M or 2G)"))?;
    Ok((n * mult as f64) as u64)
}

pub fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", units[i]) }
}

/// `mailroom coffer status|push …`
pub fn cli(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let sub = args.first().map(String::as_str);
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let dest = flag("--to")
        .or_else(|| std::env::var("MAILROOM_COFFER_REMOTE").ok())
        .ok_or("say where: --to host:/path/to/85-B_contents (or set MAILROOM_COFFER_REMOTE)")?;
    let dest = Dest::parse(&dest);
    let vault = PathBuf::from(std::env::var("MAILROOM_VAULT").map_err(|_| "set MAILROOM_VAULT")?);
    let contents = crate::coffer::resolve(&vault)?.contents;

    match sub {
        Some("status") => {
            let st = status(&contents, &dest)?;
            let only: u64 = st.only_here.iter().map(|b| b.bytes).sum();
            println!("this coffer:   {} blobs, {}", st.local, human(st.local_bytes));
            println!("{}", dest.describe());
            if st.only_here.is_empty() {
                println!("  holds a copy of every blob here");
            } else {
                println!("  is missing {} blob(s), {}: these exist only on this machine", st.only_here.len(), human(only));
            }
            if st.only_there > 0 {
                println!("  also holds {} blob(s) this machine doesn't (fine: this coffer is partial)", st.only_there);
            }
        }
        Some("push") => {
            let dry_run = args.iter().any(|a| a == "--dry-run");
            let max = flag("--max-size").map(|s| parse_size(&s)).transpose()?;
            println!("to {}{}", dest.describe(), if dry_run { " (dry run)" } else { "" });
            let r = push(&contents, &dest, max, dry_run, |b, err| match err {
                None => println!("  sent   {}  {}", &b.hash[..16], human(b.bytes)),
                Some(e) => println!("  FAILED {}  {e}", &b.hash[..16]),
            })?;
            let total = |v: &[Blob]| human(v.iter().map(|b| b.bytes).sum());
            println!("transport: {}{}", r.transport, if r.transport == "ssh" && !matches!(dest, Dest::Local(_)) {
                " (rsync not found at one end, or MAILROOM_NO_RSYNC set; every blob is still checked)"
            } else { "" });
            if dry_run {
                for b in &r.sent {
                    println!("  would send {}  {}", &b.hash[..16], human(b.bytes));
                }
            }
            println!(
                "{} {} blob(s), {}",
                if dry_run { "would send" } else { "sent" },
                r.sent.len(),
                total(&r.sent)
            );
            if !r.held_back.is_empty() {
                println!("held back {} larger than --max-size, {}", r.held_back.len(), total(&r.held_back));
            }
            if !r.failed.is_empty() {
                return Err(format!("{} blob(s) failed; run again to retry", r.failed.len()).into());
            }
        }
        _ => return Err("usage: mailroom coffer init | status --to DEST | push --to DEST [--dry-run] [--max-size 100M]".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn blob(contents: &Path, body: &str) -> String {
        let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body.as_bytes()));
        let dir = contents.join(&hash[..2]);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(&hash), body).unwrap();
        hash
    }

    #[test]
    fn destinations_parse() {
        assert_eq!(
            Dest::parse("locomotive:/home/t/Orchard/85_coffer/85-B_contents"),
            Dest::Remote { host: "locomotive".into(), path: "/home/t/Orchard/85_coffer/85-B_contents".into() }
        );
        assert_eq!(
            Dest::parse("tarobutter@100.106.125.87:coffer"),
            Dest::Remote { host: "tarobutter@100.106.125.87".into(), path: "coffer".into() }
        );
        assert_eq!(Dest::parse("/mnt/drive/coffer"), Dest::Local("/mnt/drive/coffer".into()));
        assert_eq!(Dest::parse("./backup:old"), Dest::Local("./backup:old".into()));
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("100M").unwrap(), 100 << 20);
        assert_eq!(parse_size("1.5g").unwrap(), 3 << 29);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert!(parse_size("lots").is_err());
    }

    #[test]
    fn pushing_to_a_local_drive_sends_what_is_missing_once() {
        let (here, there) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = blob(here.path(), "a small book");
        let b = blob(here.path(), "a much bigger movie, pretend");
        fs::write(here.path().join("nest"), "id = \"85-B\"\n").unwrap();
        let dest = Dest::Local(there.path().to_path_buf());

        let st = status(here.path(), &dest).unwrap();
        assert_eq!((st.local, st.only_here.len()), (2, 2), "the nest file is not a blob");

        let r = push(here.path(), &dest, Some(20), false, |_, _| {}).unwrap();
        assert_eq!(r.sent.len(), 1);
        assert_eq!(r.sent[0].hash, a, "the small one, under --max-size");
        assert_eq!(r.held_back.len(), 1);

        let r = push(here.path(), &dest, None, false, |_, _| {}).unwrap();
        assert_eq!(r.sent.iter().map(|b| b.hash.clone()).collect::<Vec<_>>(), vec![b]);
        assert!(status(here.path(), &dest).unwrap().only_here.is_empty());
        assert!(push(here.path(), &dest, None, false, |_, _| {}).unwrap().sent.is_empty(), "nothing twice");
    }

    #[test]
    fn a_dry_run_sends_nothing() {
        let (here, there) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        blob(here.path(), "x");
        let dest = Dest::Local(there.path().join("coffer"));
        let r = push(here.path(), &dest, None, true, |_, _| {}).unwrap();
        assert_eq!(r.sent.len(), 1);
        assert!(!there.path().join("coffer").exists());
    }

    /// A stand-in for ssh that runs the remote command locally: skips the
    /// options and the host, then hands the rest to sh.
    fn fake_ssh(dir: &Path) -> PathBuf {
        let p = dir.join("fake-ssh");
        fs::write(
            &p,
            "#!/bin/sh\nwhile [ \"$1\" = \"-o\" ]; do shift 2; done\nshift\nexec sh -c \"$*\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    // One test for everything that goes through MAILROOM_SSH and
    // MAILROOM_NO_RSYNC, so no other test runs while they are set.
    #[test]
    fn pushing_to_a_remote_checks_every_blob_at_the_far_end_with_either_transport() {
        let bin = tempfile::tempdir().unwrap();
        std::env::set_var("MAILROOM_SSH", fake_ssh(bin.path()));
        let have_rsync = Command::new("rsync").arg("--version").output().is_ok_and(|o| o.status.success());

        for (label, no_rsync) in [("rsync", false), ("ssh", true)] {
            if label == "rsync" && !have_rsync {
                eprintln!("rsync not installed here; skipping the rsync half");
                continue;
            }
            if no_rsync { std::env::set_var("MAILROOM_NO_RSYNC", "1") } else { std::env::remove_var("MAILROOM_NO_RSYNC") }
            let (here, there) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let remote_dir = there.path().join("Orchard copy/85-B_contents"); // a space, to test quoting
            let dest = Dest::Remote { host: "locomotive".into(), path: remote_dir.display().to_string() };

            let good = blob(here.path(), "a book");
            assert_eq!(remote_hashes(&dest).unwrap().len(), 0, "{label}: a missing folder is an empty coffer");

            // A blob that rotted here must not arrive under its name.
            let liar = blob(here.path(), "original bytes");
            fs::write(here.path().join(&liar[..2]).join(&liar), "changed").unwrap();

            let r = push(here.path(), &dest, None, false, |_, _| {}).unwrap();

            assert_eq!(r.transport, label);
            assert_eq!(r.sent.iter().map(|b| b.hash.clone()).collect::<Vec<_>>(), vec![good.clone()], "{label}");
            assert_eq!(r.failed.len(), 1, "{label}");
            assert!(r.failed[0].1.contains("bytes arrived as"), "{label}: {}", r.failed[0].1);
            assert_eq!(fs::read_to_string(remote_dir.join(&good[..2]).join(&good)).unwrap(), "a book");
            assert!(!remote_dir.join(&liar[..2]).join(&liar).exists(), "{label}: the rotted blob is not kept");
            if label == "rsync" {
                let staged: usize = walk_files(&remote_dir.join(".incoming"));
                assert_eq!(staged, 0, "staging is emptied: good moved, bad deleted");
            }
            assert!(push(here.path(), &dest, None, false, |_, _| {}).unwrap().sent.is_empty(), "{label}: nothing twice");

            // An earlier push cut off after rsync delivered a whole file but
            // before it was checked: the next push checks and places it.
            if label == "rsync" {
                let late = blob(here.path(), "arrived before the cut");
                let staged = remote_dir.join(".incoming").join(&late[..2]);
                fs::create_dir_all(&staged).unwrap();
                fs::copy(here.path().join(&late[..2]).join(&late), staged.join(&late)).unwrap();
                let r = push(here.path(), &dest, None, false, |_, _| {}).unwrap();
                assert!(r.sent.iter().any(|b| b.hash == late));
                assert!(remote_dir.join(&late[..2]).join(&late).exists());
            }
        }
        std::env::remove_var("MAILROOM_SSH");
        std::env::remove_var("MAILROOM_NO_RSYNC");
    }

    fn walk_files(dir: &Path) -> usize {
        let Ok(entries) = fs::read_dir(dir) else { return 0 };
        entries
            .filter_map(|e| e.ok())
            .map(|e| if e.path().is_dir() { walk_files(&e.path()) } else { 1 })
            .sum()
    }
}

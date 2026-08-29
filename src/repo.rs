//! Package repositories: somewhere to fetch a plugin from.
//!
//! A repository is a URL with an `index.json` under it saying what is there,
//! and one tarball per plugin beside it. That is deliberately the whole of it.
//! Anything that can serve a file over HTTPS can be a package repository —
//! raw file hosting, a static site, a directory on a machine you already run —
//! so publishing plugins is not a thing anybody has to be given permission
//! for, and neither is forking the ones that ship.
//!
//! ```json
//! { "format": 1, "repository": "textfold-plugins",
//!   "plugins": [
//!     { "id": "pyright", "version": "1.2.0", "url": "dist/pyright-1.2.0.tar.gz",
//!       "sha256": "…", "about": "Types and completions for Python" }
//!   ] }
//! ```
//!
//! The index is fetched into a cache and read from there. Nothing here waits
//! on the network at a moment anybody is looking at a cursor: the refresh
//! happens on a thread at startup, and until it lands the cached copy from
//! last time is what the lists are built out of. A machine that has never been
//! online has an empty index and everything else about textfold works, which
//! is the behaviour to hold onto.
//!
//! ## Fetching
//!
//! With `curl`, or `wget` where there is no curl. Not with an HTTP client of
//! our own: that is a TLS stack, a certificate store and a decade of other
//! people's security advisories, in a program whose job is to edit text. The
//! two programs that already do this are on every machine, they are what
//! everybody's proxy settings are already configured for, and a machine with
//! neither has told you something about itself.
//!
//! ## What is checked
//!
//! The digest in the index, against the tarball, before anything is unpacked.
//! An index served over HTTPS from somewhere you named is the thing being
//! trusted; the digest is what stops a tarball that arrived corrupted or from
//! a mirror that has drifted from being unpacked and then *run*, since a
//! plugin's install steps run programs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The repository textfold knows about when nobody has said otherwise.
pub const DEFAULT_NAME: &str = "textfold-plugins";
pub const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/Tinfold/textfold-plugins/main";

/// The newest index format this textfold understands. An index that says a
/// higher number is one written for a later textfold, and is left alone rather
/// than half-read.
const FORMAT: u32 = 1;

/// One repository, as the settings file names it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    /// What it is called here: the directory the cache goes in, and the word
    /// beside a package saying where it came from.
    pub name: String,
    /// The URL an `index.json` sits under. A trailing slash is optional.
    pub url: String,
}

impl Repository {
    /// Where `index.json` is.
    fn index_url(&self) -> String {
        format!("{}/index.json", self.url.trim_end_matches('/'))
    }

    /// Where one plugin's tarball is. A `url` in the index that is already
    /// absolute is used as it stands, so a repository can serve its index and
    /// its tarballs from different places.
    fn url_of(&self, url: &str) -> String {
        match url.starts_with("http://") || url.starts_with("https://") {
            true => url.to_string(),
            false => format!("{}/{}", self.url.trim_end_matches('/'), url.trim_start_matches('/')),
        }
    }
}

/// The repositories to use: what the settings say, or the one that ships.
///
/// Naming any replaces the default rather than adding to it. A setting that
/// silently kept something you did not write down would be a setting you
/// cannot use to say "only mine".
pub fn repositories(said: &[Repository]) -> Vec<Repository> {
    match said.is_empty() {
        true => vec![Repository {
            name: DEFAULT_NAME.to_string(),
            url: DEFAULT_URL.to_string(),
        }],
        false => said.to_vec(),
    }
}

/// One plugin, as an index describes it.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub about: Option<String>,
    pub version: String,
    /// Where the tarball is, relative to the repository or absolute.
    pub url: String,
    /// What the tarball should hash to. Checked before it is unpacked.
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// What it will want on the machine, so a list can say so before anything
    /// has been downloaded.
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub see: Option<String>,
}

/// An index, as its file writes it.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Index {
    #[serde(default)]
    pub format: u32,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub about: Option<String>,
    #[serde(default)]
    pub plugins: Vec<Entry>,
}

/// Where the fetched indexes are kept.
///
/// Beside the tools rather than beside the settings: this is a cache, it is
/// rebuilt by asking again, and throwing it away should cost nothing but a
/// download.
pub fn cache_dir() -> Option<PathBuf> {
    Some(crate::pack::tools_dir()?.parent()?.join("repositories"))
}

fn index_path(name: &str) -> Option<PathBuf> {
    // A repository named `../x` would otherwise write outside the cache.
    let safe: String = name
        .chars()
        .map(|c| match c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
            true => c,
            false => '_',
        })
        .collect();
    let safe = safe.trim_matches('.').to_string();
    (!safe.is_empty()).then(|| cache_dir().map(|d| d.join(format!("{safe}.json"))))?
}

/// What was fetched last time, or nothing.
///
/// Never an error: a missing index means a repository nobody has reached yet,
/// and that is a list with less in it rather than something to complain about.
pub fn cached(repository: &Repository) -> Option<Index> {
    let text = std::fs::read_to_string(index_path(&repository.name)?).ok()?;
    let index: Index = serde_json::from_str(&text).ok()?;
    (index.format <= FORMAT).then_some(index)
}

/// Everything every repository is offering, newest version of each id first.
pub fn offered(said: &[Repository]) -> Vec<(Repository, Entry)> {
    let mut out: Vec<(Repository, Entry)> = Vec::new();
    for repository in repositories(said) {
        let Some(index) = cached(&repository) else {
            continue;
        };
        for entry in index.plugins {
            // The first repository to offer an id is the one that gets it, so
            // the order they are written in is the order they are trusted in.
            if out.iter().any(|(_, already)| already.id == entry.id) {
                continue;
            }
            out.push((repository.clone(), entry));
        }
    }
    out
}

/// Ask a repository what it has now, and write it down.
///
/// The whole file is fetched and parsed before anything is replaced, so a
/// download that failed halfway leaves the copy from last time intact rather
/// than a truncated one that will not parse.
pub fn refresh(repository: &Repository) -> Result<usize, String> {
    let path = index_path(&repository.name)
        .ok_or_else(|| format!("{}: not a name a cache can be kept under", repository.name))?;
    let temp = path.with_extension("fetching");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    download(&repository.index_url(), &temp)?;
    let read = std::fs::read_to_string(&temp).map_err(|e| format!("{}: {e}", temp.display()));
    let parsed = read.and_then(|text| {
        serde_json::from_str::<Index>(&text)
            .map_err(|e| format!("{}: {e}", repository.index_url()))
    });
    let index = match parsed {
        Ok(index) => index,
        Err(why) => {
            std::fs::remove_file(&temp).ok();
            return Err(why);
        }
    };
    if index.format > FORMAT {
        std::fs::remove_file(&temp).ok();
        return Err(format!(
            "{} is written for a later textfold (format {})",
            repository.name, index.format
        ));
    }
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(index.plugins.len())
}

/// Fetch one plugin's tarball, check it against what the index said, and leave
/// it at `to`.
pub fn fetch(repository: &Repository, entry: &Entry, to: &Path) -> Result<(), String> {
    download(&repository.url_of(&entry.url), to)?;
    let Some(want) = entry.sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let got = sha256_of(to)?;
    if !got.eq_ignore_ascii_case(want) {
        std::fs::remove_file(to).ok();
        return Err(format!(
            "{} does not match what {} said it would be",
            entry.id, repository.name
        ));
    }
    Ok(())
}

/// Fetch a URL to a file, with whichever of the two programs is here.
///
/// `-f` matters more than it looks: without it curl writes the server's error
/// page into the file and reports success, and a 404 page unpacked as a plugin
/// is a confusing afternoon.
fn download(url: &str, to: &Path) -> Result<(), String> {
    let attempts: [(&str, Vec<String>); 2] = [
        (
            "curl",
            vec![
                "-fsSL".into(),
                "--max-time".into(),
                "60".into(),
                "-o".into(),
                to.display().to_string(),
                url.to_string(),
            ],
        ),
        (
            "wget",
            vec![
                "-q".into(),
                "--timeout=60".into(),
                "-O".into(),
                to.display().to_string(),
                url.to_string(),
            ],
        ),
    ];
    let mut last = None;
    for (program, args) in attempts {
        if !crate::pack::on_path(program) {
            continue;
        }
        let done = std::process::Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        match done {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => {
                let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
                last = Some(match said.is_empty() {
                    true => format!("{program} could not fetch {url}"),
                    false => format!("{url}: {said}"),
                });
            }
            Err(e) => last = Some(format!("{program}: {e}")),
        }
        // A program that ran and failed has answered. Trying the other one
        // would only produce the same 404 twice.
        break;
    }
    // Whatever half-arrived is not something to leave lying about looking
    // like a download that worked.
    std::fs::remove_file(to).ok();
    Err(last.unwrap_or_else(|| {
        "there is no curl and no wget here, so nothing can be fetched".to_string()
    }))
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// Whether `offered` is newer than `installed`.
///
/// Compared a piece at a time, and a piece that is a number is compared as
/// one — so `1.10.0` is newer than `1.9.0`, which is the thing string
/// comparison gets wrong and the only reason this is written out. A piece that
/// is not a number sorts before one, so `1.0.0-rc1` is older than `1.0.0`,
/// which is what everybody means by it.
pub fn is_newer(offered: &str, installed: &str) -> bool {
    order(offered, installed) == std::cmp::Ordering::Greater
}

fn order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let pieces = |v: &str| -> Vec<String> {
        v.split(['.', '-', '+', '_'])
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (a, b) = (pieces(a), pieces(b));
    for at in 0..a.len().max(b.len()) {
        // A version that ran out is the shorter one, and shorter is older —
        // except against a piece that is not a number, since `1.0` is newer
        // than `1.0-rc1` rather than older.
        let side = match (a.get(at), b.get(at)) {
            (None, None) => break,
            (None, Some(rest)) => match rest.parse::<u64>().is_ok() {
                true => Ordering::Less,
                false => Ordering::Greater,
            },
            (Some(rest), None) => match rest.parse::<u64>().is_ok() {
                true => Ordering::Greater,
                false => Ordering::Less,
            },
            (Some(one), Some(two)) => match (one.parse::<u64>(), two.parse::<u64>()) {
                (Ok(one), Ok(two)) => one.cmp(&two),
                // A number outranks anything that is not one: `1.0` is newer
                // than `1.rc`.
                (Ok(_), Err(_)) => Ordering::Greater,
                (Err(_), Ok(_)) => Ordering::Less,
                (Err(_), Err(_)) => one.cmp(two),
            },
        };
        if side != Ordering::Equal {
            return side;
        }
    }
    Ordering::Equal
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

/// The digest of a file, as the hexadecimal everybody writes it in.
///
/// Written out here rather than fetched from a crate, and rather than shelled
/// out to `sha256sum`, for the same reason it is checked at all: a check that
/// only happens on the machines that happen to have a particular program is
/// not a check. It is eighty lines and it has not changed since 2001.
fn sha256_of(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(sha256(&data))
}

pub fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (at, word) in block.chunks_exact(4).enumerate() {
            w[at] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for at in 16..64 {
            let s0 = w[at - 15].rotate_right(7) ^ w[at - 15].rotate_right(18) ^ (w[at - 15] >> 3);
            let s1 = w[at - 2].rotate_right(17) ^ w[at - 2].rotate_right(19) ^ (w[at - 2] >> 10);
            w[at] = w[at - 16]
                .wrapping_add(s0)
                .wrapping_add(w[at - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for at in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let one = hh
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[at])
                .wrapping_add(w[at]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let most = (a & b) ^ (a & c) ^ (b & c);
            let two = s0.wrapping_add(most);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(one);
            d = c;
            c = b;
            b = a;
            a = one.wrapping_add(two);
        }
        for (at, piece) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            h[at] = h[at].wrapping_add(piece);
        }
    }

    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_the_one_everybody_elses_tools_produce() {
        // The three every implementation of this is checked against, so that
        // a tarball textfold refuses is one somebody else's `sha256sum`
        // refuses too.
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // And something longer than one block, which is where a length that
        // was counted wrong shows up.
        assert_eq!(
            sha256(&b"a".repeat(1000)),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn a_newer_version_is_the_one_a_person_would_call_newer() {
        assert!(is_newer("1.1.0", "1.0.0"));
        // The one string comparison gets wrong, and the reason this is
        // written out at all.
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(is_newer("2.0", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.1"));
        // A version that ran out is the older one.
        assert!(is_newer("1.0.1", "1.0"));
        assert!(!is_newer("1.0", "1.0.1"));
        // A pre-release is older than the thing it leads up to, which is what
        // everybody means by writing one.
        assert!(is_newer("1.0.0", "1.0.0-rc1"));
        assert!(!is_newer("1.0.0-rc1", "1.0.0"));
        assert!(is_newer("1.0.0-rc2", "1.0.0-rc1"));
    }

    #[test]
    fn naming_a_repository_replaces_the_one_that_ships() {
        // A setting that quietly kept something you did not write down would
        // be a setting you cannot use to say "only mine".
        let default = repositories(&[]);
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].name, DEFAULT_NAME);

        let mine = Repository {
            name: "mine".into(),
            url: "https://example.invalid/p".into(),
        };
        assert_eq!(repositories(std::slice::from_ref(&mine)), vec![mine]);
    }

    #[test]
    fn a_url_in_an_index_is_read_against_the_repository_it_came_from() {
        let repository = Repository {
            name: "r".into(),
            url: "https://example.invalid/plugins/".into(),
        };
        assert_eq!(repository.index_url(), "https://example.invalid/plugins/index.json");
        assert_eq!(
            repository.url_of("dist/zls-1.0.0.tar.gz"),
            "https://example.invalid/plugins/dist/zls-1.0.0.tar.gz"
        );
        // One that is already absolute stands as it is, so an index and its
        // tarballs can be served from different places.
        assert_eq!(
            repository.url_of("https://elsewhere.invalid/zls.tar.gz"),
            "https://elsewhere.invalid/zls.tar.gz"
        );
    }

    #[test]
    fn a_repository_cannot_write_its_cache_outside_the_cache() {
        // The name comes out of a settings file, and a settings file is a
        // thing people paste into.
        let sneaky = index_path("../../etc/passwd").expect("still a name");
        assert_eq!(
            sneaky.parent(),
            cache_dir().as_deref(),
            "it wrote outside the cache"
        );
        assert!(index_path("...").is_none(), "a name that is all dots is no name");
    }
}

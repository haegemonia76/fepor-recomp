//! Anonymising text for bug reports (ticket #47, ADR 0047). One function, [`Anonymiser::apply`], is the only
//! place that decides what a report may say about the machine and the person; everything that goes into the
//! zip passes through it.
//!
//! What it removes, in this order: the data folder (becomes `<data>`), the folder of the disc image
//! (`<disc folder>`), the home directory, e-mail addresses, the user name and host name as words (not inside
//! other words), the machine id, MAC and IP addresses, and finally every other absolute path (`<path>`, Linux,
//! macOS and Windows spellings).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

/// What identifies the machine and the person, as far as the environment tells.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub homes: Vec<String>,
    pub user: Option<String>,
    pub host: Option<String>,
    pub machine_id: Option<String>,
    /// The folder holding the disc image or the extracted game, when it is not under the data folder.
    pub disc_dir: Option<String>,
}

impl Identity {
    /// Read the identity of this machine and user.
    pub fn from_env() -> Identity {
        let first = |vars: &[&str]| {
            vars.iter()
                .find_map(|v| std::env::var(v).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let file = |paths: &[&str]| {
            paths
                .iter()
                .find_map(|p| std::fs::read_to_string(p).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let homes = home_dirs();
        let user = first(&["USER", "LOGNAME", "USERNAME"]).or_else(|| {
            homes
                .first()
                .and_then(|h| h.rsplit(['/', '\\']).find(|s| !s.is_empty()))
                .map(str::to_string)
        });
        Identity {
            homes,
            user,
            host: first(&["HOSTNAME", "COMPUTERNAME"]).or_else(|| {
                file(&["/proc/sys/kernel/hostname", "/etc/hostname"])
            }),
            machine_id: file(&["/etc/machine-id", "/var/lib/dbus/machine-id"]),
            disc_dir: None,
        }
    }
}

/// The user's home directory as the environment states it, plus its resolved form (`/var/home/x` on
/// immutable distributions, where `/home` is a link).
pub fn home_dirs() -> Vec<String> {
    let mut homes = Vec::new();
    for var in ["HOME", "USERPROFILE"] {
        if let Some(h) = std::env::var_os(var).filter(|h| !h.is_empty()) {
            let h = std::path::PathBuf::from(h);
            if let Ok(real) = h.canonicalize() {
                homes.push(real.to_string_lossy().into_owned());
            }
            homes.push(h.to_string_lossy().into_owned());
        }
    }
    homes.dedup();
    homes
}

pub struct Anonymiser {
    id: Identity,
    data: Option<String>,
    stripped: RefCell<BTreeSet<&'static str>>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| {
        RegexBuilder::new(pattern)
            .build()
            .expect("built-in pattern is valid")
    })
}

/// Every spelling a directory takes in a log or a TOML file: `/` or `\` separators, `\\` escaped.
fn spellings(dir: &str) -> Vec<String> {
    let d = dir.trim_end_matches(['/', '\\']);
    if d.len() < 2 {
        return Vec::new();
    }
    let back = d.replace('/', "\\");
    let mut v = vec![
        d.replace('\\', "/"),
        back.replace('\\', "\\\\"),
        back,
    ];
    v.sort_by_key(|f| std::cmp::Reverse(f.len()));
    v.dedup();
    v
}

/// Replace `dir` (any spelling, only where it ends at a name boundary) with `with`; true when it did.
fn replace_dir(text: &str, dirs: &[String], with: &str) -> (String, bool) {
    let mut forms: Vec<String> = dirs.iter().flat_map(|d| spellings(d)).collect();
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms.dedup();
    let (mut out, mut rest, mut hit) = (String::with_capacity(text.len()), text, false);
    'scan: while !rest.is_empty() {
        for f in &forms {
            if let Some(after) = rest.strip_prefix(f.as_str()) {
                let more = after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
                if !more {
                    out.push_str(with);
                    rest = after;
                    hit = true;
                    continue 'scan;
                }
            }
        }
        let n = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..n]);
        rest = &rest[n..];
    }
    (out, hit)
}

/// Replace valid IPv4 addresses that are not part of a longer dotted number (a version).
fn replace_ipv4(text: &str, re: &Regex) -> (String, bool) {
    let (mut out, mut last, mut hit) = (String::with_capacity(text.len()), 0, false);
    for m in re.find_iter(text) {
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        let before = text[..m.start()].chars().rev().take(2).collect::<Vec<_>>();
        let after = text[m.end()..].chars().take(2).collect::<Vec<_>>();
        let longer = (before.first() == Some(&'.') && digit(before.get(1).copied()))
            || (after.first() == Some(&'.') && digit(after.get(1).copied()));
        let valid = m.as_str().split('.').all(|o| o.parse::<u16>().is_ok_and(|n| n <= 255));
        if valid && !longer {
            out.push_str(&text[last..m.start()]);
            out.push_str("<ip>");
            last = m.end();
            hit = true;
        }
    }
    out.push_str(&text[last..]);
    (out, hit)
}

/// Replace `word` where it is a whole word (not next to a letter or digit), ignoring case.
fn replace_word(text: &str, word: &str, with: &str) -> (String, bool) {
    let lower_word = word.to_lowercase();
    let wlen = lower_word.chars().count();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let (mut out, mut i, mut hit) = (String::with_capacity(text.len()), 0, false);
    let alnum = |c: char| c.is_alphanumeric();
    while i < chars.len() {
        let start = chars[i].0;
        let end_i = i + wlen;
        let end = chars.get(end_i).map_or(text.len(), |c| c.0);
        let boundary_before = i == 0 || !alnum(chars[i - 1].1);
        let boundary_after = chars.get(end_i).is_none_or(|c| !alnum(c.1));
        if end_i <= chars.len()
            && boundary_before
            && boundary_after
            && text[start..end].to_lowercase() == lower_word
        {
            out.push_str(with);
            hit = true;
            i = end_i;
        } else {
            out.push(chars[i].1);
            i += 1;
        }
    }
    (out, hit)
}

impl Anonymiser {
    /// `data` is the data folder: it becomes `<data>` wherever it appears.
    pub fn new(id: Identity, data: &Path) -> Anonymiser {
        Anonymiser {
            id,
            data: Some(data.to_string_lossy().into_owned()),
            stripped: RefCell::default(),
        }
    }

    /// Tell the anonymiser where the disc image is (only its extension survives, see [`Self::disc_file`]).
    pub fn with_disc_dir(mut self, dir: Option<String>) -> Anonymiser {
        self.id.disc_dir = dir;
        self
    }

    /// A disc image path reduced to what is safe to say: its extension, or `folder` for an extracted game.
    pub fn disc_file(path: &Path) -> String {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if path.is_file() => format!("<disc image>.{e}"),
            _ => "<disc folder>".into(),
        }
    }

    /// What `apply` removed so far, for the report.
    pub fn stripped(&self) -> Vec<&'static str> {
        self.stripped.borrow().iter().copied().collect()
    }

    fn note(&self, what: &'static str, hit: bool) {
        if hit {
            self.stripped.borrow_mut().insert(what);
        }
    }

    pub fn apply(&self, text: &str) -> String {
        static EMAIL: OnceLock<Regex> = OnceLock::new();
        static MAC: OnceLock<Regex> = OnceLock::new();
        static IPV6: OnceLock<Regex> = OnceLock::new();
        static WIN: OnceLock<Regex> = OnceLock::new();
        static UNIX: OnceLock<Regex> = OnceLock::new();
        let mut t = text.to_string();
        if let Some(d) = &self.data {
            let (n, hit) = replace_dir(&t, std::slice::from_ref(d), "<data>");
            (t, _) = (n, self.note("data folder", hit));
        }
        if let Some(d) = &self.id.disc_dir {
            let (n, hit) = replace_dir(&t, std::slice::from_ref(d), "<disc folder>");
            (t, _) = (n, self.note("disc image folder", hit));
        }
        let (n, hit) = replace_dir(&t, &self.id.homes, "~");
        (t, _) = (n, self.note("home directory", hit));
        let r = re(&EMAIL, r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}");
        let hit = r.is_match(&t);
        t = r.replace_all(&t, "<email>").into_owned();
        self.note("e-mail addresses", hit);
        if let Some(u) = self.id.user.as_deref().filter(|u| u.chars().count() >= 3) {
            let (n, hit) = replace_word(&t, u, "<user>");
            (t, _) = (n, self.note("user name", hit));
        }
        if let Some(h) = self
            .id
            .host
            .as_deref()
            .filter(|h| h.chars().count() >= 3 && !h.eq_ignore_ascii_case("localhost"))
        {
            let (n, hit) = replace_word(&t, h, "<host>");
            (t, _) = (n, self.note("host name", hit));
        }
        if let Some(m) = self.id.machine_id.as_deref().filter(|m| m.len() >= 16) {
            let hit = t.to_lowercase().contains(&m.to_lowercase());
            if hit {
                let (n, _) = replace_word(&t, m, "<machine id>");
                t = n;
            }
            self.note("machine id", hit);
        }
        static IPV4C: OnceLock<Regex> = OnceLock::new();
        let r = re(&MAC, r"\b(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b");
        let hit = r.is_match(&t);
        t = r.replace_all(&t, "<mac>").into_owned();
        self.note("MAC addresses", hit);
        let (n, hit) = replace_ipv4(&t, re(&IPV4C, r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b"));
        t = n;
        let r = re(&IPV6, r"\b(?:[0-9A-Fa-f]{1,4}:){4,7}[0-9A-Fa-f]{1,4}\b");
        let hit6 = r.is_match(&t);
        t = r.replace_all(&t, "<ip>").into_owned();
        self.note("IP addresses", hit || hit6);
        // paths: Windows drive and UNC paths, then Unix ones (also what is left under `~`)
        let r = re(
            &WIN,
            r#"(?:\b[A-Za-z]:[\\/]|(?:\\\\){1,2}[A-Za-z0-9._$-]+[\\/]|~\\{1,2})(?:[^\s"'<>|:*?)\],;]+)"#,
        );
        let hit = r.is_match(&t);
        t = r.replace_all(&t, "<path>").into_owned();
        self.note("paths", hit);
        let r = re(
            &UNIX,
            r"(?:^|[\s=,;\[(\x22'`])(~?/(?:[A-Za-z0-9._@+%~-]+/)*[A-Za-z0-9._@+%~-]+)",
        );
        let hit = r.is_match(&t);
        t = r
            .replace_all(&t, |c: &regex::Captures| {
                let whole = c.get(0).unwrap().as_str();
                let p = c.get(1).unwrap().as_str();
                // a lone `/name` is not a path worth hiding ("a /b" in prose); anything under `~` is
                if !p.starts_with('~') && p.matches('/').count() < 2 {
                    return whole.to_string();
                }
                whole.replacen(p, "<path>", 1)
            })
            .into_owned();
        self.note("paths", hit);
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Anonymiser {
        Anonymiser::new(
            Identity {
                homes: vec!["/home/bob".into(), "/var/home/bob".into()],
                user: Some("bob".into()),
                host: Some("bobs-pc".into()),
                machine_id: Some("0123456789abcdef0123456789abcdef".into()),
                disc_dir: Some("/mnt/games/gamecube".into()),
            },
            Path::new("/home/bob/.local/share/fepor-recomp"),
        )
    }

    #[test]
    fn linux_paths() {
        let a = linux();
        assert_eq!(
            a.apply("log /home/bob/.local/share/fepor-recomp/logs/x.log"),
            "log <data>/logs/x.log"
        );
        assert_eq!(a.apply("disc=/mnt/games/gamecube/por.iso"), "disc=<disc folder>/por.iso");
        assert_eq!(a.apply("open /home/bob/Games/other.txt now"), "open <path> now");
        assert_eq!(a.apply("lib /usr/lib64/libvulkan.so.1,"), "lib <path>,");
        assert_eq!(a.apply("(/var/home/bob/x/y)"), "(<path>)");
    }

    #[test]
    fn macos_and_windows_paths() {
        let mut id = linux().id.clone();
        id.homes = vec!["/Users/Alice".into(), "C:\\Users\\Alice".into()];
        id.user = Some("Alice".into());
        id.disc_dir = None;
        let a = Anonymiser::new(id, Path::new("/Users/Alice/Library/Application Support/fepor"));
        assert_eq!(
            a.apply("x /Users/Alice/Library/Application Support/fepor/logs/a.log"),
            "x <data>/logs/a.log"
        );
        assert_eq!(a.apply("/Users/Alice/Documents/game.iso"), "<path>");
        assert_eq!(a.apply("C:\\Users\\Alice\\Desktop\\por.iso"), "<path>");
        assert_eq!(a.apply("D:\\Games\\por\\main.dol"), "<path>");
        assert_eq!(a.apply("path = \"C:\\\\Users\\\\Alice\\\\x\""), "path = \"<path>\"");
        assert_eq!(a.apply("//server/share is not hidden but \\\\NAS\\share\\x is"), "//server/share is not hidden but <path> is");
        assert!(a.stripped().contains(&"home directory"));
    }

    #[test]
    fn the_user_name_inside_another_word_stays() {
        let a = linux();
        assert_eq!(
            a.apply("bobby Bobsled robert bob BOB bob-laptop 'bob'"),
            "bobby Bobsled robert <user> <user> <user>-laptop '<user>'"
        );
        // a longer name is not the home: it is just another path
        assert_eq!(a.apply("/home/bobby/x"), "<path>");
        assert_eq!(a.apply("on bobs-pc."), "on <host>.");
        // too short a name is left alone rather than eating text
        let mut id = linux().id.clone();
        id.user = Some("al".into());
        assert_eq!(Anonymiser::new(id, Path::new("/d")).apply("al and all"), "al and all");
    }

    #[test]
    fn addresses_ids_and_mail() {
        let a = linux();
        assert_eq!(a.apply("mail bob@example.org or x.y+z@mail.example.co.uk"), "mail <email> or <email>");
        assert_eq!(a.apply("mac aa:bb:cc:dd:ee:ff and AA-BB-CC-DD-EE-FF"), "mac <mac> and <mac>");
        assert_eq!(a.apply("ip 192.168.1.20, 10.0.0.1."), "ip <ip>, <ip>.");
        assert_eq!(a.apply("fe80:0:0:0:1:2:3:4"), "<ip>");
        assert_eq!(a.apply("id 0123456789ABCDEF0123456789abcdef"), "id <machine id>");
        // versions and times are not addresses
        assert_eq!(a.apply("Mesa 25.1.4 at 12:30:45 v1.2.3.4.5"), "Mesa 25.1.4 at 12:30:45 v1.2.3.4.5");
        // a controller GUID is not the machine id
        assert_eq!(a.apply("030000005e040000ea02000000007801"), "030000005e040000ea02000000007801");
    }

    #[test]
    fn report_lists_what_it_stripped() {
        let a = linux();
        a.apply("nothing here");
        assert!(a.stripped().is_empty());
        a.apply("/home/bob/x/y and bob@example.org");
        assert_eq!(a.stripped(), ["e-mail addresses", "home directory", "paths"]);
    }
}

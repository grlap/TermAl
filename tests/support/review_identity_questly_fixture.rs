//! A private regression repository for the Questly input classes: Flutter
//! source, byte-sensitive web evidence, CRLF scripts, binary/untracked bytes,
//! and the initialized rincon-common topology. Payloads are synthetic and
//! contain no project secrets. This is not the historical candidate/hash.
use super::fixture::{Fixture, child, commit, git, put};
use std::{fs, path::PathBuf};

pub struct QuestlyFixture {
    pub repo: Fixture,
    pub deep: PathBuf,
}

impl QuestlyFixture {
    pub fn new(ordinary_files: usize) -> Self {
        assert!(ordinary_files <= 10_000);
        let repo = Fixture::new();
        repo.put(".gitignore", b"/build/\nnode_modules/\n*.log\n");
        repo.put(
            ".gitattributes",
            b"* text=auto eol=lf\n*.cmd text eol=crlf\n*.png binary\n",
        );
        repo.put(
            ".gitmodules",
            b"[submodule \"rincon-common\"]\n\tpath = rincon-common\n\turl = https://example.invalid/fixture.git\n",
        );
        repo.put("lib/main.dart", b"void main() {}\n");
        repo.put(
            "website/en/privacy.html",
            b"<p>Local fixture evidence</p>\n",
        );
        repo.put("scripts/build.cmd", b"@echo off\r\nexit /b 0\r\n");
        repo.put(
            "assets/icon.png",
            &[137, 80, 78, 71, 13, 10, 26, 10, 0, 255],
        );
        for number in 0..ordinary_files {
            repo.put(
                &format!("lib/generated/f{number:05}.dart"),
                format!("const fixture{number} = {number};\n").as_bytes(),
            );
        }
        let (common, common_git) = child(&repo.root, &repo.root.join(".git"), "rincon-common");
        let (deep, _) = child(&common, &common_git, "packages/domain-core");
        commit(&common);
        let oid = git(&common, &["rev-parse", "HEAD"], None);
        git(
            &repo.root,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{oid},rincon-common"),
            ],
            None,
        );
        repo.commit();
        repo.put("lib/main.dart", b"void main() { /* staged */ }\n");
        git(&repo.root, &["add", "--", "lib/main.dart"], None);
        repo.put("lib/main.dart", b"void main() { /* unstaged */ }\r\n");
        repo.put(
            "docs/compliance/evidence/export.bin",
            &[0, 255, 254, 13, 10, 0],
        );
        put(&common, "tracked", b"dirty shared source\n");
        put(&deep, "tracked", b"dirty nested domain\n");
        repo.put("build/generated/ignored", b"not an input");
        repo.put("node_modules/private/ignored", b"not an input");
        // No dependency install, URL access, live store or project process.
        assert!(fs::metadata(repo.root.join(".git")).unwrap().is_dir());
        Self { repo, deep }
    }
}

/// Census-only reconstruction. Names and payload bytes are synthetic; counts,
/// sums, mode classes and staged actions reproduce the owner's metadata.
/// There are no unstaged/untracked inputs and the one submodule is clean.
pub struct QuestlyCensusFixture {
    pub repo: Fixture,
}
impl QuestlyCensusFixture {
    pub const ROOT_FILES: usize = 1667;
    pub const ROOT_BYTES: u64 = 704_424_679;
    pub const CHILD_FILES: usize = 2103;
    pub const CHILD_BYTES: u64 = 24_194_657;

    pub fn new() -> Self {
        let repo = Fixture::new();
        // Assert placement BEFORE allocating the census payload. Fixture owns
        // cleanup throughout construction and every measurement failure path.
        let project = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let scratch = fs::canonicalize(project.join(".tmp")).unwrap();
        assert_eq!(scratch, project.join(".tmp"));
        let home = fs::canonicalize(&repo.home).unwrap();
        assert_eq!(home.parent(), Some(scratch.as_path()));
        assert!(fs::canonicalize(&repo.root).unwrap().starts_with(&home));
        git(&repo.root, &["config", "core.longpaths", "true"], None);
        let policies=[
            (".gitignore",b"/build/\n*.log\n".as_slice()),
            (".gitattributes",b"* -text\n".as_slice()),
            (".gitmodules",b"[submodule \"rincon-common\"]\n\tpath = rincon-common\n\turl = https://example.invalid/fixture.git\n".as_slice()),
        ];
        for (path, bytes) in policies {
            repo.put(path, bytes);
        }
        let policy_bytes = policies.iter().map(|(_, b)| b.len() as u64).sum::<u64>();
        // 1,650 unchanged files, including the three policy files.
        const UNCHANGED_BYTES: u64 = 703_542_345;
        payload(&repo.root, "assets/capacity/largest.png", 6_065_957, 1);
        let sizes = distribute(UNCHANGED_BYTES - policy_bytes - 6_065_957, 1646);
        let mut executable = Vec::new();
        for (number, size) in sizes.into_iter().enumerate() {
            let path = if number < 10 {
                format!("assets/capacity/épreuve-{number:04}.bin")
            } else if number == 10 {
                format!("assets/{}.bin", "p".repeat(117))
            } else {
                format!("assets/capacity/asset-{number:04}.bin")
            };
            payload(&repo.root, &path, size, number + 2);
            if number < 9 {
                executable.push(path);
            }
        }
        // Old endpoints of ten modifications and two deletions, totaling
        // 550,742 bytes; initial HEAD has 1,662 files / 704,093,087 bytes.
        for (number, size) in distribute(550_742, 12).into_iter().enumerate() {
            payload(
                &repo.root,
                &format!("docs/change-{number:02}.bin"),
                size,
                2000 + number,
            );
        }
        let (common, _) = child(&repo.root, &repo.root.join(".git"), "rincon-common");
        git(&common, &["config", "core.longpaths", "true"], None);
        payload(&common, "tracked", 1_366_771, 3000);
        let mut child_executable = Vec::new();
        for (number, size) in distribute(Self::CHILD_BYTES - 1_366_771, 2102)
            .into_iter()
            .enumerate()
        {
            let path = if number == 0 {
                format!("legal/{}.bin", "q".repeat(185))
            } else {
                format!("legal/capacity/file-{number:04}.bin")
            };
            payload(&common, &path, size, 4000 + number);
            if number < 6 {
                child_executable.push(path);
            }
        }
        commit_modes(&common, &child_executable);
        let oid = git(&common, &["rev-parse", "HEAD"], None);
        git(
            &repo.root,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{oid},rincon-common"),
            ],
            None,
        );
        commit_modes(&repo.root, &executable);

        let modifications = [
            204892, 5815, 8028, 2716, 154463, 100259, 23482, 14564, 12881, 21009,
        ];
        let additions = [13930, 9640, 27, 8028, 36183, 116837, 149580];
        assert_eq!(
            modifications.iter().chain(additions.iter()).sum::<u64>(),
            882_334
        );
        for (number, size) in modifications.into_iter().enumerate() {
            payload(
                &repo.root,
                &format!("docs/change-{number:02}.bin"),
                size,
                7000 + number,
            );
        }
        for number in 10..12 {
            fs::remove_file(repo.root.join(format!("docs/change-{number:02}.bin"))).unwrap();
        }
        for (number, size) in additions.into_iter().enumerate() {
            payload(
                &repo.root,
                &format!("docs/added-{number:02}.bin"),
                size,
                8000 + number,
            );
        }
        git(&repo.root, &["add", "--", "."], None);
        // These are private fixture assertions, not queries of a live index.
        // Count index-path endpoints without content-based rename grouping.
        let changes = git(
            &repo.root,
            &["diff", "--cached", "--name-status", "--no-renames"],
            None,
        );
        assert_eq!(changes.lines().filter(|l| l.starts_with("M\t")).count(), 10);
        assert_eq!(changes.lines().filter(|l| l.starts_with("A\t")).count(), 7);
        assert_eq!(changes.lines().filter(|l| l.starts_with("D\t")).count(), 2);
        assert_census(
            &repo.root,
            Self::ROOT_FILES,
            Self::ROOT_BYTES,
            6_065_957,
            9,
            128,
        );
        assert_census(
            &common,
            Self::CHILD_FILES,
            Self::CHILD_BYTES,
            1_366_771,
            6,
            195,
        );
        Self { repo }
    }
}

fn assert_census(
    root: &std::path::Path,
    files: usize,
    bytes: u64,
    largest: u64,
    executables: usize,
    max_path: usize,
) {
    let listing = git(root, &["ls-files", "--stage", "-z"], None);
    let mut count = 0usize;
    let mut total = 0u64;
    let mut maximum = 0u64;
    let mut executable = 0usize;
    let mut longest = 0usize;
    for record in listing.split('\0').filter(|record| !record.is_empty()) {
        let (header, path) = record.split_once('\t').unwrap();
        if header.starts_with("160000 ") {
            continue;
        }
        let size = fs::metadata(root.join(path)).unwrap().len();
        count += 1;
        total += size;
        maximum = maximum.max(size);
        longest = longest.max(path.len());
        executable += usize::from(header.starts_with("100755 "));
    }
    assert_eq!(
        (count, total, maximum, executable, longest),
        (files, bytes, largest, executables, max_path)
    );
}

fn distribute(total: u64, count: usize) -> Vec<u64> {
    let quotient = total / count as u64;
    let remainder = total % count as u64;
    (0..count)
        .map(|i| quotient + u64::from((i as u64) < remainder))
        .collect()
}
fn payload(root: &std::path::Path, path: &str, size: u64, seed: usize) {
    use std::io::Write;
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(path).unwrap();
    let mut buffer = [0u8; 64 * 1024];
    buffer.fill((seed % 251) as u8);
    let label = format!("private synthetic fixture {seed}\0");
    let label_length = label.len().min(buffer.len());
    buffer[..label_length].copy_from_slice(&label.as_bytes()[..label_length]);
    let mut remaining = size;
    while remaining > 0 {
        let amount = (remaining as usize).min(buffer.len());
        file.write_all(&buffer[..amount]).unwrap();
        remaining -= amount as u64;
    }
}
fn commit_modes(root: &std::path::Path, paths: &[String]) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in paths {
            fs::set_permissions(root.join(path), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    git(root, &["add", "--", "."], None);
    let mut arguments = vec!["update-index", "--chmod=+x", "--"];
    arguments.extend(paths.iter().map(String::as_str));
    git(root, &arguments, None);
    git(
        root,
        &["commit", "--quiet", "-m", "private census fixture"],
        None,
    );
}

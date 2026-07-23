use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

const TAILWIND_VERSION: &str = "4.3.3";
const SASS_VERSION: &str = "1.101.3";
const LESS_VERSION: &str = "4.7.0";
const SOURCE_MAP_VERSION: &str = "0.6.1";

static SUITE: OnceLock<Result<Suite, String>> = OnceLock::new();
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Suite {
    repo_root: PathBuf,
    install_root: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[serde(default)]
    isolated: bool,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    name: String,
    #[serde(default)]
    args: Vec<String>,
    status: i32,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

pub struct CaseContext<'a> {
    pub workspace: &'a Path,
    pub home: &'a Path,
    pub install_root: &'a Path,
}

pub fn default_setup(_: &CaseContext<'_>) -> Result<(), String> {
    Ok(())
}

pub fn run_case(
    case_name: &str,
    setup: impl FnOnce(&CaseContext<'_>) -> Result<(), String>,
) -> Result<String, String> {
    run_case_with(case_name, setup, |_| Ok(()))
}

pub fn run_case_with(
    case_name: &str,
    setup: impl FnOnce(&CaseContext<'_>) -> Result<(), String>,
    verify: impl FnOnce(&CaseContext<'_>) -> Result<(), String>,
) -> Result<String, String> {
    let suite = SUITE
        .get_or_init(Suite::initialize)
        .as_ref()
        .map_err(Clone::clone)?;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(case_name);
    let metadata_path = fixture.join("case.toml");
    let case: Case = toml::from_str(
        &fs::read_to_string(&metadata_path)
            .map_err(|error| format!("read {}: {error}", metadata_path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", metadata_path.display()))?;
    if case.steps.is_empty() {
        return Err(format!("fixture {case_name} has no steps"));
    }
    if !fixture.join("package.json").is_file() {
        return Err(format!("fixture {case_name} must contain package.json"));
    }

    let workspace_parent = if case.isolated {
        suite
            .install_root
            .parent()
            .expect("install root must have a suite root")
            .join("isolated-workspaces")
    } else {
        suite.install_root.join("workspaces")
    };
    let case_root = unique_dir(&workspace_parent, case_name)?;
    let workspace = case_root.join("workspace");
    let home = case_root.join("home");
    let result = (|| {
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("create {}: {error}", workspace.display()))?;
        fs::create_dir_all(&home).map_err(|error| format!("create {}: {error}", home.display()))?;
        copy_tree(&fixture, &workspace, Some(&metadata_path))?;
        if workspace.join("case.toml").exists() {
            return Err(format!(
                "fixture metadata was copied into {}",
                workspace.display()
            ));
        }
        let context = CaseContext {
            workspace: &workspace,
            home: &home,
            install_root: &suite.install_root,
        };
        setup(&context)?;
        let run_result = run_steps(suite, case_name, &case, &workspace, &home);
        let verify_result = verify(&context);
        match (run_result, verify_result) {
            (Ok(rendered), Ok(())) => Ok(rendered),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(run_error), Err(verify_error)) => Err(format!(
                "{run_error}\nverification/teardown also failed: {verify_error}"
            )),
        }
    })();
    let cleanup = fs::remove_dir_all(&case_root);
    match (result, cleanup) {
        (Ok(rendered), Ok(())) => Ok(rendered),
        (Ok(_), Err(error)) => Err(format!("clean up {}: {error}", case_root.display())),
        (Err(error), _) => Err(error),
    }
}

impl Suite {
    fn initialize() -> Result<Self, String> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                "snapshot crate is not under the repository crates directory".to_string()
            })?
            .canonicalize()
            .map_err(|error| format!("resolve repository root: {error}"))?;
        // The standard test harness runs these cases in one process. A stable
        // checkout-specific root lets the next run reclaim interrupted runs
        // without unsafe process-exit hooks or an ever-growing set of temp dirs.
        let suite_root = suite_root(&repo_root);
        match fs::remove_dir_all(&suite_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("clean stale {}: {error}", suite_root.display())),
        }
        fs::create_dir(&suite_root)
            .map_err(|error| format!("create {}: {error}", suite_root.display()))?;
        let staging = suite_root.join("staging");
        let tarballs = suite_root.join("tarballs");
        let install_root = suite_root.join("install");
        fs::create_dir_all(&staging)
            .map_err(|error| format!("create {}: {error}", staging.display()))?;
        fs::create_dir_all(&tarballs)
            .map_err(|error| format!("create {}: {error}", tarballs.display()))?;
        fs::create_dir_all(&install_root)
            .map_err(|error| format!("create {}: {error}", install_root.display()))?;

        let tracked_manifest = fs::read(repo_root.join("package.json"))
            .map_err(|error| format!("read tracked package.json: {error}"))?;
        let root_stage = staging.join("root");
        stage_root_package(&repo_root, &root_stage, &tracked_manifest)?;
        let root_tarball = npm_pack(&root_stage, &tarballs)?;
        if fs::read(repo_root.join("package.json"))
            .map_err(|error| format!("re-read tracked package.json: {error}"))?
            != tracked_manifest
        {
            return Err("snapshot packaging modified tracked package.json".to_string());
        }

        let platform_dir = repo_root.join("npm").join(current_platform()?);
        let addon = platform_dir.join(current_addon()?);
        if !addon.is_file() {
            return Err(format!(
                "native release artifact is missing at {}; run `pnpm build && pnpm artifacts` first",
                addon.display()
            ));
        }
        let platform_tarball = npm_pack(&platform_dir, &tarballs)?;

        fs::write(install_root.join("package.json"), "{\"private\":true}\n")
            .map_err(|error| format!("write install package.json: {error}"))?;
        let mut install = npm_command();
        install
            .current_dir(&install_root)
            .args([
                "install",
                "--omit=optional",
                "--no-audit",
                "--no-fund",
                "--package-lock=false",
            ])
            .arg(&root_tarball)
            .arg(&platform_tarball)
            .arg(format!("tailwindcss@{TAILWIND_VERSION}"))
            .arg(format!("sass@{SASS_VERSION}"))
            .arg(format!("less@{LESS_VERSION}"))
            .arg(format!("source-map@{SOURCE_MAP_VERSION}"));
        run_setup_command(&mut install, "install packed CLI and fixture dependencies")?;

        let bin = installed_bin(&install_root);
        if !bin.is_file() {
            return Err(format!(
                "npm did not create installed CLI bin at {}",
                bin.display()
            ));
        }
        Ok(Self {
            repo_root,
            install_root,
        })
    }
}

fn stage_root_package(
    repo_root: &Path,
    stage: &Path,
    tracked_manifest: &[u8],
) -> Result<(), String> {
    fs::create_dir_all(stage).map_err(|error| format!("create {}: {error}", stage.display()))?;
    let mut manifest: Value = serde_json::from_slice(tracked_manifest)
        .map_err(|error| format!("parse tracked package.json: {error}"))?;
    let version = manifest
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| "tracked package.json has no string version".to_string())?
        .to_string();
    let optional = manifest
        .get_mut("optionalDependencies")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "tracked package.json has no optionalDependencies object".to_string())?;
    for dependency_version in optional.values_mut() {
        *dependency_version = Value::String(version.clone());
    }
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "tracked package.json has no files array".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "package files entries must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    for relative in files.into_iter().chain(["README.md", "LICENSE"]) {
        let source = repo_root.join(relative);
        if !source.exists() {
            return Err(format!(
                "publishable package path is missing: {}",
                source.display()
            ));
        }
        copy_tree(&source, &stage.join(relative), None)?;
    }
    let mut bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("serialize staged package.json: {error}"))?;
    bytes.push(b'\n');
    fs::write(stage.join("package.json"), bytes)
        .map_err(|error| format!("write staged package.json: {error}"))
}

fn npm_pack(package_dir: &Path, destination: &Path) -> Result<PathBuf, String> {
    let before = tgz_files(destination)?;
    let mut command = npm_command();
    command
        .current_dir(package_dir)
        .args(["pack", "--pack-destination"])
        .arg(destination);
    run_setup_command(&mut command, &format!("pack {}", package_dir.display()))?;
    let after = tgz_files(destination)?;
    let created = after.difference(&before).collect::<Vec<_>>();
    match created.as_slice() {
        [path] => Ok((*path).clone()),
        _ => Err(format!(
            "npm pack in {} created {} tarballs, expected one",
            package_dir.display(),
            created.len()
        )),
    }
}

fn tgz_files(directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .filter_map(|entry| match entry {
            Ok(entry) if entry.path().extension() == Some(OsStr::new("tgz")) => {
                Some(Ok(entry.path()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(format!("read {} entry: {error}", directory.display()))),
        })
        .collect()
}

fn npm_command() -> Command {
    Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
}

fn run_setup_command(command: &mut Command, operation: &str) -> Result<Output, String> {
    let debug = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| format!("{operation}: could not run {debug}: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{operation} failed with {}\ncommand: {debug}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn current_platform() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-x64"),
        ("linux", "aarch64") => Ok("linux-arm64-gnu"),
        ("linux", "x86_64") => Ok("linux-x64-gnu"),
        ("windows", "x86_64") => Ok("win32-x64-msvc"),
        (os, arch) => Err(format!("unsupported snapshot platform: {os}-{arch}")),
    }
}

fn current_addon() -> Result<&'static str, String> {
    match current_platform()? {
        "darwin-arm64" => Ok("tw-migrate.darwin-arm64.node"),
        "darwin-x64" => Ok("tw-migrate.darwin-x64.node"),
        "linux-arm64-gnu" => Ok("tw-migrate.linux-arm64-gnu.node"),
        "linux-x64-gnu" => Ok("tw-migrate.linux-x64-gnu.node"),
        "win32-x64-msvc" => Ok("tw-migrate.win32-x64-msvc.node"),
        _ => unreachable!(),
    }
}

fn installed_bin(install_root: &Path) -> PathBuf {
    install_root
        .join("node_modules")
        .join(".bin")
        .join(if cfg!(windows) {
            "tw-migrate.cmd"
        } else {
            "tw-migrate"
        })
}

fn run_steps(
    suite: &Suite,
    case_name: &str,
    case: &Case,
    workspace: &Path,
    home: &Path,
) -> Result<String, String> {
    let mut document = format!("case: {case_name}\n");
    for step in &case.steps {
        let cwd = workspace.join(step.cwd.as_deref().unwrap_or_else(|| Path::new(".")));
        validate_cwd(workspace, step.cwd.as_deref(), &cwd)?;
        let before = capture_tree(workspace)?;
        let mut command;
        if cfg!(windows) {
            command = Command::new("cmd.exe");
            command
                .args(["/d", "/c"])
                .arg(installed_bin(&suite.install_root));
        } else {
            command = Command::new(installed_bin(&suite.install_root));
        }
        command
            .current_dir(&cwd)
            .args(&step.args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("NO_COLOR", "1")
            .envs(&step.env);
        let output = command.output().map_err(|error| {
            format!(
                "case {case_name}, step {}: run installed CLI: {error}",
                step.name
            )
        })?;
        let actual_status = output.status.code().ok_or_else(|| {
            format!(
                "case {case_name}, step {}: CLI terminated without an integer exit status",
                step.name
            )
        })?;
        if actual_status != step.status {
            return Err(format!(
                "case {case_name}, step {}: expected status {}, got {}\nstdout:\n{}\nstderr:\n{}",
                step.name,
                step.status,
                actual_status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let after = capture_tree(workspace)?;
        let stdout = String::from_utf8(output.stdout).map_err(|error| {
            format!(
                "case {case_name}, step {} stdout was not UTF-8: {error}",
                step.name
            )
        })?;
        let stderr = String::from_utf8(output.stderr).map_err(|error| {
            format!(
                "case {case_name}, step {} stderr was not UTF-8: {error}",
                step.name
            )
        })?;
        document.push_str(&format!(
            "\n--- step: {} ---\nstatus: {}\nstdout: {}\nstderr: {}\nworkspace delta:\n{}",
            step.name,
            actual_status,
            quoted(&normalize_output(&stdout, suite, workspace)),
            quoted(&normalize_output(&stderr, suite, workspace)),
            render_delta(&before, &after)
        ));
    }
    Ok(document)
}

fn validate_cwd(workspace: &Path, relative: Option<&Path>, cwd: &Path) -> Result<(), String> {
    if let Some(relative) = relative {
        if relative.is_absolute()
            || relative.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "fixture cwd must stay within the workspace: {}",
                relative.display()
            ));
        }
    }
    if !cwd.is_dir() {
        return Err(format!("fixture cwd does not exist: {}", cwd.display()));
    }
    let canonical = cwd
        .canonicalize()
        .map_err(|error| format!("resolve fixture cwd {}: {error}", cwd.display()))?;
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("resolve workspace {}: {error}", workspace.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "fixture cwd escapes the workspace: {}",
            cwd.display()
        ));
    }
    Ok(())
}

fn normalize_output(value: &str, suite: &Suite, workspace: &Path) -> String {
    let mut normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let roots = [
        (workspace, "[WORKSPACE]"),
        (suite.install_root.as_path(), "[INSTALL]"),
        (suite.repo_root.as_path(), "[REPO]"),
    ];
    for (root, replacement) in roots {
        let mut spellings = vec![root.to_string_lossy().into_owned()];
        if let Ok(canonical) = root.canonicalize() {
            spellings.push(canonical.to_string_lossy().into_owned());
        }
        spellings.extend(spellings.clone().into_iter().filter_map(|spelling| {
            if let Some(path) = spelling.strip_prefix(r"\\?\UNC\") {
                Some(format!(r"\\{path}"))
            } else {
                spelling.strip_prefix(r"\\?\").map(str::to_owned)
            }
        }));
        spellings.extend(
            spellings
                .clone()
                .into_iter()
                .map(|spelling| spelling.replace('\\', "/")),
        );
        spellings.sort_by_key(|spelling| std::cmp::Reverse(spelling.len()));
        spellings.dedup();
        for spelling in spellings {
            let file_url = format!("file:///{}", spelling.trim_start_matches(['/', '\\']));
            normalized = normalized.replace(&file_url, &format!("file:{replacement}"));
            normalized = normalized.replace(&spelling, replacement);
        }
    }
    normalize_transaction_tokens(normalize_known_path_separators(normalized))
}

fn normalize_known_path_separators(mut value: String) -> String {
    for marker in ["[WORKSPACE]", "[INSTALL]", "[REPO]"] {
        let mut search_from = 0;
        while let Some(offset) = value[search_from..].find(marker) {
            let start = search_from + offset + marker.len();
            let end = value[start..]
                .find(|character: char| {
                    character.is_ascii_whitespace() || matches!(character, '\'' | '"' | ')' | ',')
                })
                .map_or(value.len(), |offset| start + offset);
            value.replace_range(start..end, &value[start..end].replace('\\', "/"));
            search_from = end;
        }
    }
    value
}

fn normalize_transaction_tokens(mut value: String) -> String {
    for marker in [".tw-migrate-backup-", ".tw-migrate-stage-"] {
        let mut search_from = 0;
        while let Some(offset) = value[search_from..].find(marker) {
            let start = search_from + offset + marker.len();
            let end = value[start..]
                .find(|character: char| !character.is_ascii_digit() && character != '-')
                .map_or(value.len(), |offset| start + offset);
            if end == start {
                search_from = start;
                continue;
            }
            value.replace_range(start..end, "[TOKEN]");
            search_from = start + "[TOKEN]".len();
        }
    }
    value
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

type Tree = BTreeMap<String, String>;

fn capture_tree(root: &Path) -> Result<Tree, String> {
    let mut tree = Tree::new();
    capture_directory(root, root, &mut tree)?;
    Ok(tree)
}

fn capture_directory(root: &Path, directory: &Path, tree: &mut Tree) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("read workspace directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read workspace directory {}: {error}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("traversed path must be under root");
        if relative
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == ".git")
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            capture_directory(root, &path, tree)?;
        } else if file_type.is_file() {
            let content = fs::read_to_string(&path)
                .map_err(|error| format!("read fixture text file {}: {error}", path.display()))?;
            tree.insert(
                normalized_relative(relative),
                content.replace("\r\n", "\n").replace('\r', "\n"),
            );
        } else if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("read symlink {}: {error}", path.display()))?;
            tree.insert(
                normalized_relative(relative),
                format!("<symlink:{}>", normalized_relative(&target)),
            );
        } else {
            return Err(format!("unsupported fixture entry: {}", path.display()));
        }
    }
    Ok(())
}

fn render_delta(before: &Tree, after: &Tree) -> String {
    let paths = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
    let mut rendered = String::new();
    for path in paths {
        match (before.get(path), after.get(path)) {
            (None, Some(content)) => {
                rendered.push_str(&format!("  added {path}\n    after: {}\n", quoted(content)))
            }
            (Some(content), None) => rendered.push_str(&format!(
                "  deleted {path}\n    before: {}\n",
                quoted(content)
            )),
            (Some(old), Some(new)) if old != new => rendered.push_str(&format!(
                "  modified {path}\n    before: {}\n    after: {}\n",
                quoted(old),
                quoted(new)
            )),
            _ => {}
        }
    }
    if rendered.is_empty() {
        "  unchanged\n".to_string()
    } else {
        rendered
    }
}

fn normalized_relative(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn copy_tree(source: &Path, destination: &Path, excluded: Option<&Path>) -> Result<(), String> {
    if excluded.is_some_and(|path| source == path) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect {}: {error}", source.display()))?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .map_err(|error| format!("create {}: {error}", destination.display()))?;
        for entry in
            fs::read_dir(source).map_err(|error| format!("read {}: {error}", source.display()))?
        {
            let entry =
                entry.map_err(|error| format!("read {} entry: {error}", source.display()))?;
            copy_tree(
                &entry.path(),
                &destination.join(entry.file_name()),
                excluded,
            )?;
        }
    } else if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        fs::copy(source, destination).map_err(|error| {
            format!(
                "copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    } else {
        return Err(format!(
            "cannot stage non-file fixture entry: {}",
            source.display()
        ));
    }
    Ok(())
}

fn suite_root(repo_root: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    repo_root.hash(&mut hasher);
    env::temp_dir().join(format!("tw-migrate-snapshots-{:016x}", hasher.finish()))
}

fn unique_dir(parent: &Path, label: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    for _ in 0..100 {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{label}-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create {}: {error}", path.display())),
        }
    }
    Err(format!(
        "could not allocate a temporary directory under {}",
        parent.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_is_ordered_and_preserves_full_contents() {
        let before = Tree::from([
            ("b.txt".to_string(), "old\n".to_string()),
            ("c.txt".to_string(), "gone\n".to_string()),
        ]);
        let after = Tree::from([
            ("a.txt".to_string(), "new\n".to_string()),
            ("b.txt".to_string(), "changed\n".to_string()),
        ]);
        assert_eq!(
            render_delta(&before, &after),
            "  added a.txt\n    after: \"new\\n\"\n  modified b.txt\n    before: \"old\\n\"\n    after: \"changed\\n\"\n  deleted c.txt\n    before: \"gone\\n\"\n"
        );
    }

    #[test]
    fn case_schema_rejects_unknown_fields() {
        let error = toml::from_str::<Case>("steps = []\nunknown = true\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn output_normalization_only_replaces_known_roots_and_separators() {
        let suite = Suite {
            repo_root: PathBuf::from(r"C:\repo"),
            install_root: PathBuf::from(r"C:\repo\install"),
        };
        let workspace = Path::new(r"C:\repo\install\workspaces\case");
        assert_eq!(
            normalize_output(
                "C:\\repo\\install\\workspaces\\case\\nested\\file.css\r\nfile:///C:/repo/install/workspaces/case/url.css\nC:\\repo\\other.css escaped\\_value\n",
                &suite,
                workspace,
            ),
            "[WORKSPACE]/nested/file.css\nfile:[WORKSPACE]/url.css\n[REPO]/other.css escaped\\_value\n"
        );
    }

    #[test]
    fn output_normalization_strips_windows_verbatim_prefixes() {
        let suite = Suite {
            repo_root: PathBuf::from(r"\\?\C:\repo"),
            install_root: PathBuf::from(r"\\?\C:\repo\install"),
        };
        let workspace = Path::new(r"\\?\C:\repo\install\workspaces\case");
        assert_eq!(
            normalize_output(
                r"C:\repo\install\workspaces\case\file.css",
                &suite,
                workspace,
            ),
            "[WORKSPACE]/file.css"
        );

        let suite = Suite {
            repo_root: PathBuf::from(r"\\?\UNC\server\share\repo"),
            install_root: PathBuf::from(r"\\?\UNC\server\share\repo\install"),
        };
        let workspace = Path::new(r"\\?\UNC\server\share\repo\install\workspaces\case");
        assert_eq!(
            normalize_output(
                r"\\server\share\repo\install\workspaces\case\file.css",
                &suite,
                workspace,
            ),
            "[WORKSPACE]/file.css"
        );
    }

    #[test]
    fn output_normalization_replaces_transaction_tokens() {
        assert_eq!(
            normalize_transaction_tokens(
                "a/.file.tw-migrate-backup-123-456-0' b/.file.tw-migrate-stage-9-8-7\n".to_string()
            ),
            "a/.file.tw-migrate-backup-[TOKEN]' b/.file.tw-migrate-stage-[TOKEN]\n"
        );
    }
}

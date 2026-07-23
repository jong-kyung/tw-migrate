use super::*;
use std::fs;
use std::path::Path;
use std::process::Command;
use tw_migrate_snapshots::{CaseContext, copy_tree, run_case_with};

snapshot_cases! {
    safety_gitignored_html_entity => git_init,
    safety_explicit_gitignored_target => git_init,
    safety_reference_only_workspace => git_init,
    safety_workspace_consumer => git_init,
    safety_workspace_html_consumer => git_init,
    safety_gitignored_unparseable => git_init,
    safety_gitignored_named_module => git_init,
    safety_gitignored_consumer => git_init,
    safety_gitignored_relationship => git_init,
    safety_gitignored_composes => git_init,
    safety_workspace_candidate_failure => git_init,
    safety_force_workspace_skip => git_init,
    safety_force_collision => git_init,
    safety_force_malformed => default_setup,
    safety_interrupted_leftovers => git_init,
    safety_symlink_rejection => setup_symlinks,
    safety_planning_mutation => setup_planning_mutation,
    safety_reference_mutation => setup_reference_mutation,
    safety_missing_sass => setup_missing_sass,
    safety_post_edit_sass => setup_fake_sass,
    safety_missing_less => setup_missing_less,
    safety_post_edit_less => setup_fake_less,
}

#[test]
fn safety_permissions() {
    assert_case_with("safety_permissions", setup_permissions, verify_permissions);
}

#[cfg(unix)]
#[test]
fn safety_write_rollback() {
    assert_case_with("safety_write_rollback", setup_rollback, verify_rollback);
}

fn assert_case_with(
    case: &str,
    setup: impl FnOnce(&CaseContext<'_>) -> Result<(), String>,
    verify: impl FnOnce(&CaseContext<'_>) -> Result<(), String>,
) {
    let document = run_case_with(case, setup, verify).unwrap_or_else(|error| panic!("{error}"));
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots"));
    settings.bind(|| insta::assert_snapshot!(case, document));
}

fn git_init(context: &CaseContext<'_>) -> Result<(), String> {
    let gitignore_fixture = context.workspace.join(".gitignore.fixture");
    if gitignore_fixture.is_file() {
        fs::rename(&gitignore_fixture, context.workspace.join(".gitignore"))
            .map_err(|error| format!("install fixture .gitignore: {error}"))?;
    }
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(context.workspace)
        .output()
        .map_err(|error| format!("run git init: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn install_tailwind_only(context: &CaseContext<'_>, missing: &str) -> Result<(), String> {
    let modules = context.workspace.join("node_modules");
    fs::create_dir_all(&modules)
        .map_err(|error| format!("create {}: {error}", modules.display()))?;
    copy_tree(
        &context.install_root.join("node_modules/tailwindcss"),
        &modules.join("tailwindcss"),
        None,
    )?;
    let script = format!(
        "require.resolve('tailwindcss/package.json'); try {{ require.resolve({missing:?}); process.exit(2) }} catch (error) {{ if (error.code !== 'MODULE_NOT_FOUND') throw error }}"
    );
    let output = Command::new("node")
        .args(["-e", &script])
        .current_dir(context.workspace)
        .output()
        .map_err(|error| format!("preflight isolated dependencies: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "isolated dependency preflight failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn setup_missing_sass(context: &CaseContext<'_>) -> Result<(), String> {
    install_tailwind_only(context, "sass")
}

fn setup_missing_less(context: &CaseContext<'_>) -> Result<(), String> {
    install_tailwind_only(context, "less")
}

fn setup_fake_sass(context: &CaseContext<'_>) -> Result<(), String> {
    install_tailwind_only(context, "missing-compiler-preflight")?;
    create_file_symlink(
        Path::new("Button.module.scss"),
        &context.workspace.join("Mapped.module.scss"),
    )?;
    let real = context.install_root.join("node_modules/sass");
    let source = format!(
        "import {{ createRequire }} from 'node:module';\nimport {{ resolve }} from 'node:path';\nimport {{ pathToFileURL }} from 'node:url';\nconst require = createRequire(import.meta.url);\nconst sass = require({});\nlet calls = 0;\nexport const compileStringAsync = async (...args) => {{\n  calls += 1;\n  const result = await sass.compileStringAsync(...args);\n  if (calls > 1 && process.env.FAKE_RESULT === 'throw') throw new Error('post-edit compile failed');\n  if (calls > 1 && process.env.FAKE_RESULT === 'malformed') return {{ ...result, css: '}}' }};\n  return {{ ...result, sourceMap: {{ ...result.sourceMap, sources: [pathToFileURL(resolve('Mapped.module.scss')).href] }} }};\n}};\n",
        serde_json::to_string(&real.to_string_lossy()).unwrap()
    );
    write_fake_package(context, "sass", &source)
}

fn setup_fake_less(context: &CaseContext<'_>) -> Result<(), String> {
    install_tailwind_only(context, "missing-compiler-preflight")?;
    let real = context.install_root.join("node_modules/less");
    let source = format!(
        "import {{ createRequire }} from 'node:module';\nconst require = createRequire(import.meta.url);\nconst less = require({});\nlet calls = 0;\nexport default {{ render: async (...args) => {{\n  calls += 1;\n  const result = await less.render(...args);\n  if (calls > 1 && process.env.FAKE_RESULT === 'throw') throw new Error('post-edit render failed');\n  return calls > 1 && process.env.FAKE_RESULT === 'malformed' ? {{ ...result, css: '}}' }} : result;\n}} }};\n",
        serde_json::to_string(&real.to_string_lossy()).unwrap()
    );
    write_fake_package(context, "less", &source)
}

fn write_fake_package(context: &CaseContext<'_>, name: &str, source: &str) -> Result<(), String> {
    let root = context.workspace.join("node_modules").join(name);
    fs::create_dir_all(&root).map_err(|error| format!("create {}: {error}", root.display()))?;
    fs::write(
        root.join("package.json"),
        "{\"type\":\"module\",\"exports\":\"./index.js\"}\n",
    )
    .map_err(|error| format!("write fake {name} package: {error}"))?;
    fs::write(root.join("index.js"), source)
        .map_err(|error| format!("write fake {name} compiler: {error}"))
}

fn setup_planning_mutation(context: &CaseContext<'_>) -> Result<(), String> {
    git_init(context)?;
    let target = context.workspace.join("bbb/globals.css");
    fs::write(
        context.workspace.join("aaa/mutate.cjs"),
        format!(
            "const fs = require('node:fs');\nfs.appendFileSync({}, '/* mutated */\\n');\nmodule.exports = () => {{}};\n",
            serde_json::to_string(&target.to_string_lossy()).unwrap()
        ),
    )
    .map_err(|error| format!("write planning mutation plugin: {error}"))
}

fn setup_reference_mutation(context: &CaseContext<'_>) -> Result<(), String> {
    git_init(context)?;
    let target = context.workspace.join("external/Note.tsx");
    fs::write(
        context.workspace.join("mutate.cjs"),
        format!(
            "const fs = require('node:fs');\nfs.appendFileSync({}, '// changed during planning\\n');\nmodule.exports = () => {{}};\n",
            serde_json::to_string(&target.to_string_lossy()).unwrap()
        ),
    )
    .map_err(|error| format!("write reference mutation plugin: {error}"))
}

fn setup_symlinks(context: &CaseContext<'_>) -> Result<(), String> {
    create_file_symlink(
        Path::new("real.module.css"),
        &context.workspace.join("linked.module.css"),
    )?;
    create_directory_symlink(
        Path::new("real-directory"),
        &context.workspace.join("linked-directory"),
    )
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|error| format!("create symlink {}: {error}", link.display()))
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_file(target, link)
        .map_err(|error| format!("create symlink {}: {error}", link.display()))
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|error| format!("create symlink {}: {error}", link.display()))
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_dir(target, link)
        .map_err(|error| format!("create symlink {}: {error}", link.display()))
}

fn setup_permissions(context: &CaseContext<'_>) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            context.workspace.join("Button.tsx"),
            fs::Permissions::from_mode(0o751),
        )
        .map_err(|error| format!("set source permissions: {error}"))?;
    }
    Ok(())
}

fn verify_permissions(context: &CaseContext<'_>) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(context.workspace.join("Button.tsx"))
            .map_err(|error| format!("inspect source permissions: {error}"))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o751 {
            return Err(format!("source mode changed: expected 751, got {mode:o}"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn setup_rollback(context: &CaseContext<'_>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(context.workspace, fs::Permissions::from_mode(0o555))
        .map_err(|error| format!("make workspace read-only: {error}"))
}

#[cfg(unix)]
fn verify_rollback(context: &CaseContext<'_>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(context.workspace, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("restore workspace permissions: {error}"))?;
    let component = fs::read_to_string(context.workspace.join("components/Button.tsx"))
        .map_err(|error| format!("read restored component: {error}"))?;
    let stylesheet = fs::read_to_string(context.workspace.join("Button.module.css"))
        .map_err(|error| format!("read restored stylesheet: {error}"))?;
    if component
        != "import styles from '../Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
        || stylesheet != ".button { padding: 13px; }\n"
    {
        return Err("rollback did not restore original contents".to_string());
    }
    let leftovers = fs::read_dir(context.workspace.join("components"))
        .map_err(|error| format!("read components after rollback: {error}"))?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().contains(".tw-migrate-"));
    if leftovers {
        return Err("rollback left transaction files behind".to_string());
    }
    Ok(())
}

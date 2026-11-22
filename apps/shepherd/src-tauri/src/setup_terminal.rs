use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use shep_core::worktree_environment::{WorktreeSetup, checkout_step_id};

const CAPTURE: &str = "--shepherd-capture-environment";
const LAUNCH: &str = "--shepherd-launch-in-environment";
pub const OVERRIDES: &str = "SHEPHERD_ENVIRONMENT_OVERRIDES";

pub fn dispatch_helper() -> Option<io::Result<()>> {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next()?.to_str()? {
        CAPTURE => Some((|| {
            let path = required(arguments.next())?;
            let mut file = tempfile::NamedTempFile::new_in(
                Path::new(&path).parent().ok_or_else(invalid_arguments)?,
            )?;
            for (name, value) in std::env::vars_os() {
                if reusable(&name) {
                    file.write_all(name.as_bytes())?;
                    file.write_all(b"=")?;
                    file.write_all(value.as_bytes())?;
                    file.write_all(b"\0")?;
                }
            }
            file.persist(path)?;
            Ok(())
        })()),
        LAUNCH => Some((|| {
            let directory = PathBuf::from(required(arguments.next())?);
            let program = required(arguments.next())?;
            let mut command = std::process::Command::new(program);
            command.args(arguments);
            apply_environment(&mut command, &directory)?;
            Err(command.exec())
        })()),
        _ => None,
    }
}

fn apply_environment(command: &mut std::process::Command, directory: &Path) -> io::Result<()> {
    let baseline = read_environment(&directory.join("baseline"))?;
    let environment = read_environment(&directory.join("environment"))?;
    let overrides: Vec<String> = std::env::var(OVERRIDES)
        .ok()
        .map(|names| serde_json::from_str(&names))
        .transpose()?
        .unwrap_or_default();
    let overridden = |name: &OsStr| overrides.iter().any(|key| name == OsStr::new(key));
    command.env_remove(OVERRIDES);
    for name in baseline.keys() {
        if !overridden(name) && !environment.contains_key(name) {
            command.env_remove(name);
        }
    }
    for (name, value) in &environment {
        if !overridden(name) && baseline.get(name) != Some(value) {
            command.env(name, value);
        }
    }
    Ok(())
}

fn required(argument: Option<OsString>) -> io::Result<OsString> {
    argument.ok_or_else(invalid_arguments)
}

fn invalid_arguments() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid environment helper arguments",
    )
}

fn reusable(name: &OsStr) -> bool {
    !name.as_bytes().starts_with(b"SHEPHERD_")
        && !matches!(
            name.to_str(),
            Some("TERM" | "COLORTERM" | "PWD" | "OLDPWD" | "SHLVL" | "_")
        )
}

fn read_environment(path: &Path) -> io::Result<BTreeMap<OsString, OsString>> {
    fs::read(path)?
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let separator = entry
                .iter()
                .position(|byte| *byte == b'=')
                .ok_or_else(invalid_arguments)?;
            Ok((
                OsString::from_vec(entry[..separator].to_vec()),
                OsString::from_vec(entry[separator + 1..].to_vec()),
            ))
        })
        .collect()
}

pub(crate) fn wrapper(directory: &Path) -> io::Result<Vec<String>> {
    wrapper_with_executable(directory, &std::env::current_exe()?)
}

fn wrapper_with_executable(directory: &Path, executable: &Path) -> io::Result<Vec<String>> {
    Ok(vec![
        path_argument(executable)?,
        LAUNCH.into(),
        path_argument(directory)?,
    ])
}

fn path_argument(path: &Path) -> io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(invalid_arguments)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_command(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn write_script(
    directory: &Path,
    checkout_state: &Path,
    setup: &WorktreeSetup,
) -> io::Result<PathBuf> {
    write_script_with_executable(directory, checkout_state, setup, &std::env::current_exe()?)
}

fn write_script_with_executable(
    directory: &Path,
    checkout_state: &Path,
    setup: &WorktreeSetup,
    executable: &Path,
) -> io::Result<PathBuf> {
    fs::create_dir_all(checkout_state)?;
    let executable_argument = path_argument(executable)?;
    let capture = |name| -> io::Result<String> {
        Ok(shell_command(&[
            executable_argument.clone(),
            CAPTURE.into(),
            path_argument(&directory.join(name))?,
        ]))
    };
    let mut login = setup
        .setup_terminal
        .as_ref()
        .map(|terminal| terminal.command.clone())
        .unwrap_or_default();
    if login.is_empty() {
        login.push(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
        login.push("-i".into());
    }
    let mut script = format!(
        "trap : INT\n{} || exit\nprepare() {{\n",
        capture("baseline")?
    );
    for (index, step) in setup.after_checkout.iter().enumerate() {
        let marker = quote(&path_argument(
            &checkout_state.join(checkout_step_id(index, step)?),
        )?);
        let command = shell_command(&step.command);
        let label = quote(&format!("{}\n$ {command}", step.name));
        let on_failure = if step.required { "return 1" } else { ":" };
        script.push_str(&format!(
            "if [ ! -f {marker} ]; then\n  printf '%s\\n' {label}\n  {command} || {{ printf '%s\\n' 'Setup command failed.'; {on_failure}; }}\n  : > {marker} || return 1\nfi\n"
        ));
    }
    if let Some(step) = &setup.readiness_check {
        let command = shell_command(&step.command);
        script.push_str(&format!(
            "printf '%s\\n' {}\n{command} || return 1\n",
            quote(&format!("{}\n$ {command}", step.name))
        ));
    }
    let capture_environment = capture("environment")?;
    if !setup.launch_wrapper.is_empty() {
        let wrapper = shell_command(&setup.launch_wrapper);
        script.push_str(&format!(
            "printf '%s\\n' {}\n{wrapper} {capture_environment}\n",
            quote(&format!("Loading environment with {wrapper}"))
        ));
    } else {
        script.push_str(&format!("{capture_environment}\n"));
    }
    let snapshot = quote(&path_argument(&directory.join("environment"))?);
    let ready = quote(&path_argument(&directory.join("ready"))?);
    let failed = quote(&path_argument(&directory.join("failed"))?);
    let instructions = setup
        .setup_terminal
        .as_ref()
        .map(|terminal| terminal.instructions.as_str())
        .unwrap_or_default();
    let login_command = shell_command(&login);
    let mut launch_login = wrapper_with_executable(directory, executable)?;
    launch_login.extend(login);
    script.push_str(&format!(
        "}}\nwhile :; do\n  rm -f {failed} {snapshot} || exit\n  if prepare && [ -f {snapshot} ]; then\n    : > {ready} || exit\n    printf '%s\\n' 'Environment ready.'\n    exec {}\n  fi\n  : > {failed} || exit\n  printf '%s\\n' 'Environment setup failed.' {} 'Fix the issue in this terminal, then exit the shell to retry setup.'\n  {login_command}\n  printf '%s\\n' 'Press Enter to retry setup.'\n  read -r retry || exit\ndone\n",
        shell_command(&launch_login),
        quote(instructions)
    ));
    let path = directory.join("setup.sh");
    fs::File::options()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)?
        .write_all(script.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::worktree_environment::{SetupCommand, SetupTerminal};
    use tokio::io::AsyncWriteExt as _;

    #[tokio::test]
    async fn retry_reuses_the_setup_terminal_and_exports_to_later_launches()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path();
        let helper = path.join("helper with 'quote");
        fs::File::options().write(true).create_new(true).mode(0o700).open(&helper)?
            .write_all(br#"#!/bin/sh
case "$1" in
  --shepherd-capture-environment)
    if [ -n "$SETUP_VALUE" ]; then printf 'SETUP_VALUE=%s\000' "$SETUP_VALUE" > "$2"; else : > "$2"; fi ;;
  --shepherd-launch-in-environment)
    shift 2
    exec "$@" ;;
esac
"#)?;
        let setup = WorktreeSetup {
            after_checkout: vec![SetupCommand {
                name: "Once per worktree".into(),
                command: vec!["/bin/sh".into(), "-c".into(), "printf x >> checkout-count".into()],
                required: true,
            }],
            launch_wrapper: vec![
                "/bin/sh".into(), "-c".into(),
                "test -f fixed || exit 1; printf x >> wrapper-count; export SETUP_VALUE='first=second'; exec \"$@\"".into(),
                "setup-fixture".into(),
            ],
            setup_terminal: Some(SetupTerminal {
                command: vec!["/bin/sh".into(), "-c".into(), "if [ ! -f fixed ]; then IFS= read -r fix && touch \"$fix\"; fi".into()],
                instructions: "Enter the fix".into(),
                ..SetupTerminal::default()
            }),
            ..WorktreeSetup::default()
        };
        let script = write_script_with_executable(path, &path.join("state"), &setup, &helper)?;
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg(script)
            .current_dir(path)
            .env_remove("SETUP_VALUE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !path.join("failed").is_file() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        child
            .stdin
            .as_mut()
            .ok_or("setup terminal is missing stdin")?
            .write_all(b"fixed\n\n")
            .await?;
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
                .await??;
        assert!(output.status.success(), "{output:?}");
        let output = String::from_utf8(output.stdout)?;
        assert!(output.contains("Environment setup failed."));
        assert!(output.contains("Environment ready."));
        assert!(path.join("ready").is_file());

        for name in ["agent", "task"] {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", "printf '%s:%s' \"$SETUP_VALUE\" \"$1\"", "test", name]);
            apply_environment(&mut command, path)?;
            assert_eq!(
                String::from_utf8(command.output()?.stdout)?,
                format!("first=second:{name}")
            );
        }
        assert_eq!(fs::read(path.join("checkout-count"))?, b"x");
        assert_eq!(fs::read(path.join("wrapper-count"))?, b"x");
        Ok(())
    }
}
